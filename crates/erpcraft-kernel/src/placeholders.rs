//! A statement built at run time must bind exactly the values its text
//! uses. sqlx prepares each statement once per connection and caches it by
//! its text alone, sending the types of the values bound the first time.
//! Postgres gives the statement one parameter per type sent, used or not.
//! So if the same text is ever run with a value it does not use, and again
//! without it, whichever runs second on a connection fails with "bind
//! message supplies N parameters, but prepared statement requires M".
//!
//! Every query whose text and arguments are assembled together (lists,
//! row filters, compiled conditions) goes through [`checked`]: the distinct
//! placeholders `$1`, `$2`, ... in its text must be exactly `$1` to `$n`
//! for `n` values bound.
//!
//! Placeholders are counted in the text as Postgres would read them:
//! `$` followed by digits, not inside a string literal (`'...'`, also with
//! an `E` prefix), a quoted identifier (`"..."`), a dollar-quoted string
//! (`$tag$...$tag$`) or a comment, and not part of an identifier
//! (`a$1`). Values are always bound, never written into the text, so a
//! literal holding `$1` cannot arise from input; it is skipped all the same.

use std::collections::BTreeSet;

use erpcraft_core::{Error, Result};
use sqlx::Arguments;
use sqlx::postgres::PgArguments;

/// The distinct placeholder numbers in `sql`.
pub fn numbers(sql: &str) -> BTreeSet<usize> {
    let bytes = sql.as_bytes();
    let mut found = BTreeSet::new();
    let mut i = 0;
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                // `E'...'` also escapes with a backslash.
                let escapes = i > 0 && matches!(bytes[i - 1], b'E' | b'e');
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' if escapes => i += 2,
                        b'\'' if bytes.get(i + 1) == Some(&b'\'') => i += 2,
                        b'\'' => break,
                        _ => i += 1,
                    }
                }
                i += 1;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' if bytes.get(i + 1) == Some(&b'"') => i += 2,
                        b'"' => break,
                        _ => i += 1,
                    }
                }
                i += 1;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b'$' if i > 0 && ident(bytes[i - 1]) => i += 1,
            b'$' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let start = i + 1;
                i = start;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if let Ok(n) = sql[start..i].parse() {
                    found.insert(n);
                }
            }
            b'$' => {
                // A dollar-quoted string: `$$...$$` or `$tag$...$tag$`.
                let tag_end = bytes[i + 1..]
                    .iter()
                    .position(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
                    .map(|n| i + 1 + n);
                match tag_end {
                    Some(end) if bytes[end] == b'$' => {
                        let tag = &sql[i..=end];
                        i = match sql[end + 1..].find(tag) {
                            Some(at) => end + 1 + at + tag.len(),
                            None => bytes.len(),
                        };
                    }
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    found
}

/// Why `sql` cannot run with `count` values bound, if it cannot.
pub fn mismatch(sql: &str, count: usize) -> Option<String> {
    let used = numbers(sql);
    let expected: BTreeSet<usize> = (1..=count).collect();
    if used == expected {
        return None;
    }
    let unused: Vec<String> = expected
        .difference(&used)
        .map(|n| format!("${n}"))
        .collect();
    let unbound: Vec<String> = used
        .difference(&expected)
        .map(|n| format!("${n}"))
        .collect();
    let mut problem = format!("the statement binds {count} values");
    if !unused.is_empty() {
        problem.push_str(&format!(" but does not use {}", unused.join(", ")));
    }
    if !unbound.is_empty() {
        let joiner = if unused.is_empty() { " but" } else { " and" };
        problem.push_str(&format!("{joiner} uses unbound {}", unbound.join(", ")));
    }
    Some(problem)
}

/// Refuse to run `sql` with `count` values unless its placeholders are
/// exactly `$1` to `$count`. A mismatch is a bug in the code that built
/// the statement: debug and test builds panic on it, release builds log it
/// and fail the request rather than send the statement.
pub fn check(sql: &str, count: usize) -> Result<()> {
    let Some(problem) = mismatch(sql, count) else {
        return Ok(());
    };
    debug_assert!(false, "{problem}: {sql}");
    tracing::error!(%problem, sql, "a statement's placeholders do not match its values");
    Err(Error::internal(problem))
}

/// `arguments`, once [`check`] has found `sql` to use exactly them.
pub fn checked(sql: &str, arguments: PgArguments) -> Result<PgArguments> {
    check(sql, arguments.len())?;
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(numbers: &[usize]) -> BTreeSet<usize> {
        numbers.iter().copied().collect()
    }

    #[test]
    fn placeholders_are_counted_once_each() {
        assert_eq!(numbers("SELECT 1"), set(&[]));
        assert_eq!(
            numbers("SELECT * FROM t WHERE a = $1 AND (b ILIKE $2 OR c ILIKE $2) LIMIT $3"),
            set(&[1, 2, 3])
        );
        assert_eq!(
            numbers("x = $10::uuid AND y = ANY($2::text[])"),
            set(&[2, 10])
        );
    }

    #[test]
    fn placeholders_are_not_found_in_literals_identifiers_or_comments() {
        assert_eq!(numbers("SELECT '$1', 'it''s $2', $3"), set(&[3]));
        assert_eq!(numbers(r"SELECT E'\'$1', $2"), set(&[2]));
        assert_eq!(
            numbers(r#"SELECT "a$1" FROM "x""$2" WHERE b = $3"#),
            set(&[3])
        );
        assert_eq!(numbers("SELECT a$1 FROM t WHERE b = $2"), set(&[2]));
        assert_eq!(numbers("SELECT $$ $1 $$, $tag$ $2 $tag$, $3"), set(&[3]));
        assert_eq!(numbers("SELECT $1 -- and $2\n, /* $3 */ $4"), set(&[1, 4]));
        assert_eq!(numbers("t.summary->>'total' = $1::text"), set(&[1]));
    }

    #[test]
    fn values_must_match_placeholders_exactly() {
        assert_eq!(mismatch("SELECT 1", 0), None);
        assert_eq!(mismatch("a = $1 AND b = $2", 2), None);
        assert_eq!(mismatch("a = $2 AND b = $1 AND c = $1", 2), None);
        assert_eq!(
            mismatch("SELECT count(*) FROM company AS t WHERE true", 1).unwrap(),
            "the statement binds 1 values but does not use $1"
        );
        assert_eq!(
            mismatch("a = $1 AND b = $3", 3).unwrap(),
            "the statement binds 3 values but does not use $2"
        );
        assert_eq!(
            mismatch("a = $1 AND b = $2", 1).unwrap(),
            "the statement binds 1 values but uses unbound $2"
        );
        assert_eq!(
            mismatch("a = $2", 1).unwrap(),
            "the statement binds 1 values but does not use $1 and uses unbound $2"
        );
    }

    #[test]
    fn matching_arguments_pass_through() {
        let mut arguments = PgArguments::default();
        arguments.add("x").unwrap();
        assert_eq!(checked("a = $1", arguments).unwrap().len(), 1);
        assert!(check("SELECT 1", 0).is_ok());
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "does not use $1")]
    fn a_mismatch_panics_in_debug_builds() {
        let _ = check("SELECT count(*) FROM company AS t WHERE true", 1);
    }
}
