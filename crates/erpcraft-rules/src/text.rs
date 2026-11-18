//! Tests on text: `record.name.contains('bolt')` and its kin.
//!
//! Each is a method of a text, with one text argument:
//!
//! | method                         | true when the text           | SQL      |
//! |--------------------------------|------------------------------|----------|
//! | `contains(s)`                  | has `s` in it                | `LIKE`   |
//! | `startsWith(s)`                | begins with `s`              | `LIKE`   |
//! | `endsWith(s)`                  | ends with `s`                | `LIKE`   |
//! | `containsIgnoreCase(s)`        | has `s` in it, in any case   | `ILIKE`  |
//! | `startsWithIgnoreCase(s)`      | begins with `s`, in any case | `ILIKE`  |
//! | `endsWithIgnoreCase(s)`        | ends with `s`, in any case   | `ILIKE`  |
//! | `equalsIgnoreCase(s)`          | is `s`, in any case          | `ILIKE`  |
//!
//! The first three are CEL's own and keep its meaning: case counts. The
//! argument is always a value, bound as a parameter; `%`, `_` and `\` in it
//! are escaped, so they match themselves and never act as wildcards.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    Anywhere,
    Start,
    End,
    Whole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextTest {
    pub position: Position,
    pub ignore_case: bool,
}

impl TextTest {
    /// The test a method name stands for, if it is one.
    pub(crate) fn named(name: &str) -> Option<TextTest> {
        let (position, ignore_case) = match name {
            "contains" => (Position::Anywhere, false),
            "startsWith" => (Position::Start, false),
            "endsWith" => (Position::End, false),
            "containsIgnoreCase" => (Position::Anywhere, true),
            "startsWithIgnoreCase" => (Position::Start, true),
            "endsWithIgnoreCase" => (Position::End, true),
            "equalsIgnoreCase" => (Position::Whole, true),
            _ => return None,
        };
        Some(TextTest {
            position,
            ignore_case,
        })
    }

    pub(crate) fn sql_operator(self) -> &'static str {
        if self.ignore_case { "ILIKE" } else { "LIKE" }
    }

    /// The `LIKE` pattern that finds `needle` where this test looks.
    pub(crate) fn pattern(self, needle: &str) -> String {
        let mut escaped = String::with_capacity(needle.len() + 2);
        for c in needle.chars() {
            if matches!(c, '%' | '_' | '\\') {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        match self.position {
            Position::Anywhere => format!("%{escaped}%"),
            Position::Start => format!("{escaped}%"),
            Position::End => format!("%{escaped}"),
            Position::Whole => escaped,
        }
    }

    /// The same test in memory.
    pub(crate) fn holds(self, text: &str, needle: &str) -> bool {
        let (text, needle) = if self.ignore_case {
            (text.to_lowercase(), needle.to_lowercase())
        } else {
            (text.to_owned(), needle.to_owned())
        };
        match self.position {
            Position::Anywhere => text.contains(&needle),
            Position::Start => text.starts_with(&needle),
            Position::End => text.ends_with(&needle),
            Position::Whole => text == needle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_in_the_needle_match_themselves() {
        let contains = TextTest::named("contains").unwrap();
        assert_eq!(contains.pattern("50%_off\\"), "%50\\%\\_off\\\\%");
        let equals = TextTest::named("equalsIgnoreCase").unwrap();
        assert_eq!(equals.pattern("a_b"), "a\\_b");
        assert_eq!(equals.sql_operator(), "ILIKE");
    }

    #[test]
    fn in_memory_as_in_sql() {
        let t = |name: &str, text: &str, needle: &str| {
            TextTest::named(name).unwrap().holds(text, needle)
        };
        assert!(t("contains", "Acme Corp", "Corp"));
        assert!(!t("contains", "Acme Corp", "corp"));
        assert!(t("containsIgnoreCase", "Acme Corp", "CORP"));
        assert!(t("startsWithIgnoreCase", "Acme", "ac"));
        assert!(t("endsWith", "Acme", "me"));
        assert!(t("equalsIgnoreCase", "ÄBC", "äbc"));
        assert!(!t("equalsIgnoreCase", "abc", "ab"));
    }
}
