//! What the kernel reads from the database about an entity's table: how
//! many decimal places each numeric column keeps, and which columns its
//! unique and check constraints are about.
//!
//! It is read once per table and kept, because the registry and the
//! schema do not change while the server runs. Reading it before a write,
//! not after one failed, matters: once a statement fails, Postgres refuses
//! every further query in that transaction.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use erpcraft_core::{Error, Result};
use erpcraft_db::PgConnection;

use crate::registry::EntityMeta;

/// A `CHECK` constraint of a table.
#[derive(Debug)]
struct Check {
    /// The columns the condition refers to.
    columns: Vec<String>,
    /// The condition, tidied for a person; `None` if it is too long.
    rule: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct TableInfo {
    /// Decimal places kept, by column, for numeric columns that declare it.
    scales: HashMap<String, u32>,
    /// Columns by name of unique index. `None`: the index is over an
    /// expression, not plain columns.
    unique: HashMap<String, Option<Vec<String>>>,
    checks: HashMap<String, Check>,
    /// The referring columns, by name of foreign key.
    references: HashMap<String, Vec<String>>,
    /// The value a column takes when none is given, where the table states
    /// a constant.
    defaults: HashMap<String, serde_json::Value>,
}

impl TableInfo {
    /// How many decimal places `column` keeps, if it is a numeric column
    /// with a fixed number of them.
    pub(crate) fn scale(&self, column: &str) -> Option<u32> {
        self.scales.get(column).copied()
    }

    /// The constant `column` defaults to, if the table states one.
    pub(crate) fn default(&self, column: &str) -> Option<&serde_json::Value> {
        self.defaults.get(column)
    }

    async fn load(conn: &mut PgConnection, table: &str) -> Result<TableInfo> {
        let defaults: Vec<(String, String)> = sqlx::query_as(
            "SELECT a.attname::text, pg_get_expr(d.adbin, d.adrelid)
             FROM pg_attrdef d
             JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
             WHERE d.adrelid = to_regclass($1::text)",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        let scales: Vec<(String, i32)> = sqlx::query_as(
            "SELECT c.column_name::text, c.numeric_scale::int4
             FROM information_schema.columns c
             JOIN pg_namespace n ON n.nspname = c.table_schema
             JOIN pg_class r ON r.relnamespace = n.oid AND r.relname = c.table_name
             WHERE r.oid = to_regclass($1::text)
               AND c.data_type = 'numeric' AND c.numeric_scale IS NOT NULL",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        let unique: Vec<(String, Option<Vec<String>>, i64)> = sqlx::query_as(
            "SELECT x.relname::text,
                    array_agg(a.attname::text ORDER BY k.ord) FILTER (WHERE a.attname IS NOT NULL),
                    count(*)
             FROM pg_index i
             JOIN pg_class x ON x.oid = i.indexrelid
             CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
             LEFT JOIN pg_attribute a
                    ON a.attrelid = i.indrelid AND a.attnum = k.attnum AND k.attnum > 0
             WHERE i.indrelid = to_regclass($1::text) AND i.indisunique
               AND k.ord <= i.indnkeyatts
             GROUP BY x.relname",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        let constraints: Vec<(String, Vec<String>, String, bool)> = sqlx::query_as(
            "SELECT c.conname::text,
                    ARRAY(SELECT a.attname::text FROM pg_attribute a
                          WHERE a.attrelid = c.conrelid AND a.attnum = ANY(c.conkey)
                          ORDER BY a.attnum),
                    pg_get_constraintdef(c.oid), c.contype = 'f'
             FROM pg_constraint c
             WHERE c.conrelid = to_regclass($1::text) AND c.contype IN ('c', 'f')",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        Ok(TableInfo {
            defaults: defaults
                .into_iter()
                .filter_map(|(column, expression)| Some((column, constant(&expression)?)))
                .collect(),
            scales: scales
                .into_iter()
                .filter_map(|(column, scale)| Some((column, u32::try_from(scale).ok()?)))
                .collect(),
            unique: unique
                .into_iter()
                .map(|(name, columns, keys)| {
                    let columns = columns.filter(|c| c.len() as i64 == keys);
                    (name, columns)
                })
                .collect(),
            checks: constraints
                .iter()
                .filter(|(.., foreign)| !foreign)
                .map(|(name, columns, definition, _)| {
                    let (columns, rule) = (columns.clone(), readable_rule(definition));
                    (name.clone(), Check { columns, rule })
                })
                .collect(),
            references: constraints
                .into_iter()
                .filter(|(.., foreign)| *foreign)
                .map(|(name, columns, ..)| (name, columns))
                .collect(),
        })
    }
}

/// The value of a column default as Postgres prints it, if it is a
/// constant: `true`, `0`, `'open'::text`. A default that is computed, such
/// as `now()`, has no value to show.
fn constant(expression: &str) -> Option<serde_json::Value> {
    let text = expression.trim();
    match text {
        "true" => return Some(true.into()),
        "false" => return Some(false.into()),
        _ => {}
    }
    if let Some(rest) = text.strip_prefix('\'') {
        // A quoted literal, with or without a cast after it.
        let end = rest.find('\'')?;
        let after = &rest[end + 1..];
        // Nothing may follow but the name of a type.
        let plain = after.strip_prefix("::").is_none_or(|ty| {
            ty.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '[' | ']' | '"'))
        }) && (after.is_empty() || after.starts_with("::"));
        return (plain && !rest[..end].contains('\\')).then(|| rest[..end].into());
    }
    // A number, which Postgres may wrap and cast: `(0)::numeric`.
    let number = text.split("::").next()?.trim_matches(['(', ')']);
    let numeric = !number.is_empty()
        && number
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-');
    if !numeric {
        return None;
    }
    match number.parse::<i64>() {
        Ok(whole) => Some(whole.into()),
        // Decimals travel as strings.
        Err(_) => Some(number.into()),
    }
}

/// What is known about each table, read on first use.
#[derive(Debug, Default)]
pub(crate) struct TableCache(RwLock<HashMap<String, Arc<TableInfo>>>);

impl TableCache {
    pub(crate) async fn get(&self, conn: &mut PgConnection, table: &str) -> Result<Arc<TableInfo>> {
        let known = self
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(table)
            .cloned();
        if let Some(info) = known {
            return Ok(info);
        }
        // Two requests may both read a table the first time; they read the
        // same thing.
        let info = Arc::new(TableInfo::load(conn, table).await?);
        self.0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(table.to_owned(), info.clone());
        Ok(info)
    }

    /// Forget everything, because the schema may have changed.
    pub(crate) fn clear(&self) {
        self.0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// Turn a failed write of an entity's record into an error that names the
/// fields involved, where the database says which constraint of the
/// entity's own table was violated. Anything else, including the plain
/// sentences the project's triggers raise, is passed on as it is.
pub(crate) fn describe(meta: &EntityMeta, info: &TableInfo, error: sqlx::Error) -> Error {
    let sqlx::Error::Database(db) = &error else {
        return error.into();
    };
    let (Some(code), Some(constraint)) = (db.code(), db.constraint()) else {
        return error.into();
    };
    if db.table() != Some(meta.table.as_str()) {
        return error.into();
    }
    match code.as_ref() {
        // unique_violation
        "23505" => match info.unique.get(constraint) {
            Some(Some(columns)) => {
                let labels: Vec<&str> = columns.iter().map(|c| label(meta, c)).collect();
                let error = Error::duplicate(format!(
                    "{} with this {} already exists",
                    noun(meta),
                    join(&labels)
                ));
                // The last column is the one that tells records apart; the
                // others, such as the company, say within what.
                match columns.last() {
                    Some(column) => error.with_field(column),
                    None => error,
                }
            }
            Some(None) => {
                Error::duplicate(format!("{} with the same value already exists", noun(meta)))
            }
            None => error.into(),
        },
        // foreign_key_violation, on a table that refers to another: what
        // one of the record's own fields points at does not exist.
        "23503" => match info.references.get(constraint) {
            Some(columns) if !columns.is_empty() => {
                let labels: Vec<&str> = columns.iter().map(|c| label(meta, c)).collect();
                let verb = if labels.len() == 1 { "refers" } else { "refer" };
                let error = Error::validation(format!(
                    "{} {verb} to a record that does not exist",
                    join(&labels)
                ));
                match columns.as_slice() {
                    [column] => error.with_field(column),
                    _ => error,
                }
            }
            _ => error.into(),
        },
        // check_violation
        "23514" => match info.checks.get(constraint) {
            Some(check) if !check.columns.is_empty() => {
                let names: Vec<String> = check.columns.iter().map(|c| format!("'{c}'")).collect();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let (fields, verb, they) = if names.len() == 1 {
                    ("field", "was", "it")
                } else {
                    ("fields", "were", "they")
                };
                let mut message = format!("{fields} {} {verb} refused", join(&names));
                if let Some(rule) = &check.rule {
                    message.push_str(&format!(": {they} must satisfy {rule}"));
                }
                let error = Error::validation(message);
                match check.columns.as_slice() {
                    [column] => error.with_field(column),
                    _ => error,
                }
            }
            // The database's own sentence, without the constraint's name.
            _ => Error::validation(
                db.message()
                    .replace(&format!(" check constraint \"{constraint}\""), " a rule"),
            ),
        },
        _ => error.into(),
    }
}

/// The label of the field stored in `column`, or the column's name if the
/// entity does not declare it.
fn label<'a>(meta: &'a EntityMeta, column: &'a str) -> &'a str {
    meta.fields
        .iter()
        .find(|f| f.name == column)
        .map_or(column, |f| f.label.as_str())
}

/// The entity as it reads inside a sentence: "an account", "a bill of
/// materials line". A label that starts with an acronym keeps its capitals.
fn noun(meta: &EntityMeta) -> String {
    let mut chars = meta.label.chars();
    let label = match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if !second.is_uppercase() => {
            format!(
                "{}{}",
                first.to_lowercase(),
                &meta.label[first.len_utf8()..]
            )
        }
        _ => meta.label.clone(),
    };
    let article = match label.chars().next() {
        Some(c) if "aeiouAEIOU".contains(c) => "an",
        _ => "a",
    };
    format!("{article} {label}")
}

/// "A", "A and B", "A, B and C".
fn join(parts: &[&str]) -> String {
    match parts {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// The condition of a check constraint as Postgres prints it, with the
/// noise removed: `CHECK ((qty > (0)::numeric))` becomes `qty > 0`.
fn readable_rule(definition: &str) -> Option<String> {
    let mut rule = definition.strip_prefix("CHECK (")?;
    rule = &rule[..rule.rfind(')')?];
    while let Some(inner) = unwrapped(rule) {
        rule = inner;
    }

    // Drop type casts and the parentheses Postgres puts around a bare
    // number, leaving quoted text alone.
    let bytes = rule.as_bytes();
    let mut out = String::with_capacity(rule.len());
    let (mut i, mut quoted) = (0, false);
    while i < bytes.len() {
        let rest = &rule[i..];
        if bytes[i] == b'\'' {
            quoted = !quoted;
        } else if !quoted && rest.starts_with("::") {
            i += 2 + cast_len(&rule[i + 2..]);
            continue;
        } else if !quoted && bytes[i] == b'(' && !out.ends_with(is_word) {
            let number = rest[1..]
                .find(')')
                .map(|end| &rest[1..=end])
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
            if let Some(number) = number {
                out.push_str(number);
                i += number.len() + 2;
                continue;
            }
        }
        // Step over one whole character.
        let width = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..width]);
        i += width;
    }
    (out.len() <= 120).then_some(out)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `(a > b)` without its parentheses, if they enclose all of it.
fn unwrapped(rule: &str) -> Option<&str> {
    let inner = rule.strip_prefix('(')?.strip_suffix(')')?;
    let (mut depth, mut quoted) = (0i32, false);
    for c in inner.chars() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    (depth == 0).then_some(inner)
}

/// The length of the type name at the start of `text`, as it follows `::`.
fn cast_len(text: &str) -> usize {
    let word = |from: usize| {
        text[from..]
            .find(|c: char| !is_word(c) && c != '"')
            .unwrap_or(text.len() - from)
    };
    let mut len = word(0);
    for more in [
        " varying",
        " precision",
        " with time zone",
        " without time zone",
    ] {
        if text[len..].starts_with(more) {
            len += more.len();
        }
    }
    // A length or precision, then array brackets.
    if text[len..].starts_with('(')
        && let Some(end) = text[len..].find(')')
    {
        len += end + 1;
    }
    while text[len..].starts_with("[]") {
        len += 2;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_conditions_are_tidied() {
        for (definition, rule) in [
            ("CHECK ((qty > (0)::numeric))", "qty > 0"),
            ("CHECK ((code <> ''::text))", "code <> ''"),
            ("CHECK ((end_date >= start_date))", "end_date >= start_date"),
            (
                "CHECK (((decimals >= 0) AND (decimals <= 4)))",
                "(decimals >= 0) AND (decimals <= 4)",
            ),
            (
                "CHECK ((kind = ANY (ARRAY['a'::text, 'b (c)::x'::character varying(5)])))",
                "kind = ANY (ARRAY['a', 'b (c)::x'])",
            ),
            (
                "CHECK ((round(rate, 2) < (100.5)::numeric(20,6)))",
                "round(rate, 2) < 100.5",
            ),
            (
                "CHECK ((jsonb_typeof(attributes) = 'object'::text))",
                "jsonb_typeof(attributes) = 'object'",
            ),
        ] {
            assert_eq!(readable_rule(definition).as_deref(), Some(rule));
        }
        assert_eq!(readable_rule("FOREIGN KEY (a) REFERENCES b(id)"), None);
    }

    #[test]
    fn constant_defaults_are_read() {
        use serde_json::json;
        for (expression, value) in [
            ("true", json!(true)),
            ("false", json!(false)),
            ("0", json!(0)),
            ("(0)::numeric", json!(0)),
            ("1.5", json!("1.5")),
            ("'open'::text", json!("open")),
            ("'{}'::jsonb", json!("{}")),
            ("2", json!(2)),
        ] {
            assert_eq!(constant(expression), Some(value), "{expression}");
        }
        for expression in [
            "now()",
            "gen_random_uuid()",
            "CURRENT_DATE",
            "'a'::text || 'b'::text",
        ] {
            assert_eq!(constant(expression), None, "{expression}");
        }
    }

    #[test]
    fn entities_read_as_nouns() {
        let noun = |label: &str| noun(&EntityMeta::new("x", label));
        assert_eq!(noun("Account"), "an account");
        assert_eq!(noun("Bill of materials line"), "a bill of materials line");
        assert_eq!(noun("GL entry"), "a GL entry");
        assert_eq!(join(&["A", "B", "C"]), "A, B and C");
    }
}
