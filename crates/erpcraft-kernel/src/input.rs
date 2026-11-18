//! Reading what a caller sent into a typed value, with an error a person
//! can act on: which field, on which line, and what is wrong with it.

use erpcraft_core::{Error, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_path_to_error::Segment;

/// Read `value` as a `T`. `what` names the value in the error: `input` for
/// the input of a command, `journal entry body` for the body of a document.
///
/// The error says where the problem is. In `{"lines": [{}, {"qty": "x"}]}`
/// a bad `qty` is reported as line 2 of the collection `lines`, field
/// `qty`, both in the sentence and in the error's detail.
pub fn parse<T: DeserializeOwned>(value: Value, what: &str) -> Result<T> {
    serde_path_to_error::deserialize(value).map_err(|error| {
        let mut place = Place::of(error.path());
        let problem = error.inner().to_string();
        let sentence = match classify(&problem) {
            Problem::Missing(field) => {
                place.push(field);
                format!("field '{}' is required", place.field_name())
            }
            Problem::Unknown(field, expected) => {
                // The path already ends at the unknown field.
                if place.field.last().map(String::as_str) != Some(field) {
                    place.push(field);
                }
                match expected {
                    Some(expected) => {
                        format!(
                            "unknown field '{}'; expected {expected}",
                            place.field_name()
                        )
                    }
                    None => format!("unknown field '{}'", place.field_name()),
                }
            }
            Problem::Other(reason) if place.field.is_empty() => reason,
            Problem::Other(reason) => {
                format!("field '{}' is not valid: {reason}", place.field_name())
            }
        };
        let (sentence, located) = match &place.line {
            Some((_, line)) => (format!("line {line}: {sentence}"), true),
            None => (sentence, false),
        };
        let mut error = Error::validation(format!("invalid {what}: {sentence}"));
        if let (Some((collection, line)), true) = (&place.line, located) {
            error = error.with_line(collection.clone(), *line);
        }
        if !place.field.is_empty() {
            error = error.with_field(place.field_name());
        }
        error
    })
}

/// Where in the value the problem is.
#[derive(Default)]
struct Place {
    /// The collection and the line in it, counted from 1.
    line: Option<(String, usize)>,
    /// The field, as the keys leading to it: within the line if there is
    /// one, otherwise from the top.
    field: Vec<String>,
}

impl Place {
    fn of(path: &serde_path_to_error::Path) -> Place {
        let mut place = Place::default();
        for segment in path.iter() {
            match segment {
                Segment::Map { key } => place.field.push(key.clone()),
                // The first list on the way is the collection; the keys
                // after it name the field within the line.
                Segment::Seq { index } if place.line.is_none() && !place.field.is_empty() => {
                    place.line = Some((place.field.join("."), index + 1));
                    place.field.clear();
                }
                Segment::Seq { index } => {
                    let last = place.field.pop().unwrap_or_default();
                    place.field.push(format!("{last}[{index}]"));
                }
                Segment::Enum { .. } | Segment::Unknown => {}
            }
        }
        place
    }

    fn push(&mut self, field: &str) {
        self.field.push(field.to_owned());
    }

    fn field_name(&self) -> String {
        self.field.join(".")
    }
}

enum Problem<'a> {
    /// A required field is absent.
    Missing(&'a str),
    /// A field that does not exist, and what serde says exists.
    Unknown(&'a str, Option<String>),
    Other(String),
}

/// Turn what serde says into what a person needs to read. serde's
/// sentences are stable enough to recognize the common ones; anything
/// else is passed on in its own words.
fn classify(problem: &str) -> Problem<'_> {
    /// The name between backticks at the start, and what follows it.
    fn quoted(rest: &str) -> Option<(&str, &str)> {
        rest.strip_prefix('`')?.split_once('`')
    }
    if let Some((field, _)) = problem.strip_prefix("missing field ").and_then(quoted) {
        return Problem::Missing(field);
    }
    if let Some((field, rest)) = problem.strip_prefix("unknown field ").and_then(quoted) {
        let expected = rest
            .strip_prefix(", expected ")
            .map(|list| list.replace('`', "'"));
        return Problem::Unknown(field, expected);
    }
    if problem.starts_with("UUID parsing failed") {
        return Problem::Other(
            "it must be the id of a record, such as 0198c5a1-7b1e-7cc3-9d6f-2f4a8f0e6b11".into(),
        );
    }
    // What chrono says about a date or a time it cannot read.
    const DATE_PROBLEMS: &[&str] = &[
        "input contains invalid characters",
        "premature end of input",
        "trailing input",
        "input is out of range",
        "no possible date and time matching input",
        "input is not enough for unique date and time",
    ];
    if DATE_PROBLEMS.contains(&problem) {
        return Problem::Other(
            "it must be a date written as 2026-03-15, or a time as 2026-03-15T10:30:00Z".into(),
        );
    }
    // From here on serde quotes the value it could not read. It is never
    // repeated: the caller knows what they sent, and what they sent may be
    // a secret or somebody else's data, while the sentence goes on into
    // the audit log. What is said is what was expected, and for a wrong
    // type what kind of value came instead. The value is always before the
    // last ", expected ", so nothing of it can be taken for what follows.
    if let Some((got, expected)) = problem
        .strip_prefix("invalid type: ")
        .and_then(|rest| rest.rsplit_once(", expected "))
    {
        return Problem::Other(format!("expected {expected}, not {}", kind_of(got)));
    }
    if let Some((_, expected)) = problem
        .strip_prefix("invalid value: ")
        .and_then(|rest| rest.rsplit_once(", expected "))
    {
        return Problem::Other(format!("expected {expected}"));
    }
    if let Some((_, expected)) = problem
        .strip_prefix("unknown variant ")
        .and_then(|rest| rest.rsplit_once(", expected "))
    {
        return Problem::Other(format!("expected {}", expected.replace('`', "'")));
    }
    // Any other sentence is passed on only if it quotes nothing: serde and
    // the types it reads put a value they repeat between quotes or
    // backticks.
    if problem.contains(['"', '`', '\'']) {
        return Problem::Other("it does not have the expected form".into());
    }
    Problem::Other(problem.to_owned())
}

/// The kind of value serde says it got, without the value itself: serde
/// writes `string "abc"`, `integer `5``, `map`.
fn kind_of(got: &str) -> &'static str {
    match got.split([' ', '`', '"']).next().unwrap_or_default() {
        "string" | "character" => "text",
        "integer" => "a whole number",
        "floating" => "a number",
        "boolean" => "true or false",
        "null" | "unit" | "Option" => "null",
        "sequence" => "a list",
        "map" => "an object",
        _ => "another kind of value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;
    use uuid::Uuid;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Body {
        id: Uuid,
        date: Option<chrono::NaiveDate>,
        #[serde(default)]
        lines: Vec<Line>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Line {
        account_id: Uuid,
        qty: Option<i32>,
    }

    fn refused(value: Value) -> Value {
        parse::<Body>(value, "input").unwrap_err().to_json()
    }

    #[test]
    fn problems_are_located_and_put_in_words() {
        let id = "0198c5a1-7b1e-7cc3-9d6f-2f4a8f0e6b11";
        let error = refused(json!({ "id": "nope" }));
        assert_eq!(error["field"], "id");
        assert_eq!(
            error["message"],
            "invalid input: field 'id' is not valid: it must be the id of a record, \
             such as 0198c5a1-7b1e-7cc3-9d6f-2f4a8f0e6b11"
        );

        let error = refused(json!({}));
        assert_eq!(error["field"], "id");
        assert_eq!(error["message"], "invalid input: field 'id' is required");

        let error = refused(json!({ "id": id, "extra": 1 }));
        assert_eq!(error["field"], "extra");
        assert_eq!(
            error["message"],
            "invalid input: unknown field 'extra'; expected one of 'id', 'date', 'lines'"
        );

        let error = refused(json!({ "id": id, "date": "tomorrow" }));
        assert_eq!(error["field"], "date");

        let error = refused(json!({ "id": id, "lines": [{ "account_id": id }, { "qty": 2 }] }));
        assert_eq!(
            error["message"],
            "invalid input: line 2: field 'account_id' is required"
        );
        assert_eq!(error["collection"], "lines");
        assert_eq!(error["line"], 2);
        assert_eq!(error["field"], "account_id");

        let error = refused(json!({ "id": id, "lines": [{ "account_id": id, "qty": "two" }] }));
        assert_eq!(
            error["message"],
            "invalid input: line 1: field 'qty' is not valid: expected i32, not text"
        );
        assert_eq!(
            (&error["line"], &error["field"]),
            (&json!(1), &json!("qty"))
        );

        // What was sent is never repeated, whatever its type, and wherever
        // serde would have quoted it.
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "snake_case")]
        #[allow(dead_code)]
        enum Mode {
            Fast,
            Slow,
        }
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Typed {
            flag: Option<bool>,
            count: Option<u8>,
            amount: Option<erpcraft_core::Decimal>,
            mode: Option<Mode>,
            name: Option<String>,
            tags: Option<Vec<String>>,
            when: Option<chrono::NaiveDate>,
            id: Option<Uuid>,
        }
        const SECRET: &str = "hunter2, expected `a secret`";
        for (field, value, said) in [
            ("flag", json!(SECRET), "expected a boolean, not text"),
            (
                "flag",
                json!(424242),
                "expected a boolean, not a whole number",
            ),
            ("flag", json!(4242.42), "expected a boolean, not a number"),
            ("flag", json!([SECRET]), "expected a boolean, not a list"),
            (
                "flag",
                json!({ SECRET: 1 }),
                "expected a boolean, not an object",
            ),
            ("count", json!(424242), "expected u8"),
            ("count", json!(SECRET), "expected u8, not text"),
            ("mode", json!(SECRET), "expected 'fast' or 'slow'"),
            (
                "name",
                json!(424242),
                "expected a string, not a whole number",
            ),
            ("name", json!(true), "expected a string, not true or false"),
            ("tags", json!(SECRET), "expected a sequence, not text"),
            (
                "tags",
                json!([424242]),
                "expected a string, not a whole number",
            ),
        ] {
            let error = parse::<Typed>(json!({ field: value }), "input")
                .unwrap_err()
                .to_json();
            let message = error["message"].as_str().unwrap();
            assert!(message.ends_with(said), "{field}: {message}");
            let place = error["field"].as_str().or(error["collection"].as_str());
            assert!(place.unwrap_or_default().starts_with(field), "{error}");
            for part in ["hunter2", "a secret", "424242", "4242.42"] {
                assert!(!message.contains(part), "{field}: {message}");
            }
        }
        for (field, value) in [
            ("amount", json!(SECRET)),
            ("amount", json!(true)),
            ("when", json!(SECRET)),
            ("when", json!(424242)),
            ("id", json!(SECRET)),
            ("id", json!(424242)),
        ] {
            let message = parse::<Typed>(json!({ field: value }), "input")
                .unwrap_err()
                .to_string();
            for part in ["hunter2", "a secret", "424242"] {
                assert!(!message.contains(part), "{field}: {message}");
            }
            assert!(message.contains(&format!("field '{field}'")), "{message}");
        }

        // Not an object at all: nothing to locate.
        let error = refused(json!("text"));
        assert_eq!(error.get("field"), None);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .starts_with("invalid input: ")
        );
    }
}
