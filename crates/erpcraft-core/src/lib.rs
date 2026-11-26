//! Shared types for erpcraft: errors, identifiers, and money rounding.

pub use chrono::{DateTime, NaiveDate, Utc};
pub use rust_decimal::Decimal;
pub use uuid::Uuid;

use rust_decimal::RoundingStrategy;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The one error type every layer returns. Transports map it to their own
/// status codes; see [`Error::code`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request is well-formed but breaks a rule.
    #[error("{0}")]
    Validation(Message),
    #[error("{0} not found")]
    NotFound(String),
    /// The request conflicts with the current state of the record.
    #[error("{0}")]
    Conflict(Message),
    #[error("{0}")]
    Forbidden(Message),
    #[error("not authenticated")]
    Unauthorized,
    #[error("internal error: {0}")]
    Internal(String),
}

/// The sentence of an error, with what is known about where it applies.
/// The sentence is complete without the detail: an interface that shows
/// only the message loses nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub detail: Detail,
}

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl<T: Into<String>> From<T> for Message {
    fn from(text: T) -> Self {
        Message {
            text: text.into(),
            detail: Detail::default(),
        }
    }
}

/// Where an error applies and why, for an interface to act on: mark a
/// field, point at a line, offer to reload. Every part is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detail {
    /// The field the error is about. In a document body, a field of the
    /// body or, with `line`, of that line.
    pub field: Option<String>,
    /// The collection of a document body that `line` counts in, for
    /// example `lines`.
    pub collection: Option<String>,
    /// The line the error is about, counted from 1.
    pub line: Option<u32>,
    /// Why a request conflicts with the current state.
    pub reason: Option<Reason>,
}

/// The kinds of conflict an interface treats differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The record changed after the caller read it: read it again.
    Stale,
    /// The record is in a state that does not allow this.
    State,
    /// A record with the same unique value exists.
    Duplicate,
    /// Something still refers to the record.
    InUse,
    /// Another change to the same records was under way at the same
    /// moment (a deadlock, or a serialization failure) and nothing was
    /// done: the same request again is expected to go through.
    Retry,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Stale => "stale",
            Reason::State => "state",
            Reason::Duplicate => "duplicate",
            Reason::InUse => "in_use",
            Reason::Retry => "retry",
        }
    }
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation(msg.into().into())
    }

    /// A validation error about one field. The message reads
    /// `field '<name>' <msg>`: `Error::field("rate", "has at most 10
    /// decimal places")`.
    pub fn field(name: &str, msg: impl std::fmt::Display) -> Self {
        Error::validation(format!("field '{name}' {msg}")).with_field(name)
    }

    /// A validation error about one line of a collection in a document
    /// body, counted from 1, and the field of that line if it is about
    /// one. The message reads `line <n>: <msg>`:
    /// `Error::line("lines", 3, Some("qty"), "the quantity must be more
    /// than zero")`.
    pub fn line(
        collection: &str,
        line: usize,
        field: Option<&str>,
        msg: impl std::fmt::Display,
    ) -> Self {
        let error = Error::validation(format!("line {line}: {msg}")).with_line(collection, line);
        match field {
            Some(field) => error.with_field(field),
            None => error,
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into().into())
    }

    /// A conflict because the record changed after the caller read it.
    pub fn stale(msg: impl Into<String>) -> Self {
        Error::conflict(msg).with_reason(Reason::Stale)
    }

    /// A conflict because the record or document is in a state that does
    /// not allow the request.
    pub fn state(msg: impl Into<String>) -> Self {
        Error::conflict(msg).with_reason(Reason::State)
    }

    /// A conflict because a record with the same unique value exists.
    pub fn duplicate(msg: impl Into<String>) -> Self {
        Error::conflict(msg).with_reason(Reason::Duplicate)
    }

    /// A conflict because something still refers to the record.
    pub fn in_use(msg: impl Into<String>) -> Self {
        Error::conflict(msg).with_reason(Reason::InUse)
    }

    /// A conflict with a change made at the same moment; nothing was done
    /// and the request may be sent again.
    pub fn retry(msg: impl Into<String>) -> Self {
        Error::conflict(msg).with_reason(Reason::Retry)
    }

    /// Whether this is a [`Error::retry`].
    pub fn is_retry(&self) -> bool {
        matches!(self, Error::Conflict(m) if m.detail.reason == Some(Reason::Retry))
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Error::Forbidden(msg.into().into())
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Error::NotFound(what.into())
    }

    pub fn internal(err: impl std::fmt::Display) -> Self {
        Error::Internal(err.to_string())
    }

    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Validation(_) => "validation",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::Forbidden(_) => "forbidden",
            Error::Unauthorized => "unauthorized",
            Error::Internal(_) => "internal",
        }
    }

    /// What is known about where the error applies. `None` for the kinds
    /// of error that carry no detail.
    pub fn detail(&self) -> Option<&Detail> {
        match self {
            Error::Validation(m) | Error::Conflict(m) | Error::Forbidden(m) => Some(&m.detail),
            _ => None,
        }
    }

    fn detail_mut(&mut self) -> Option<&mut Detail> {
        match self {
            Error::Validation(m) | Error::Conflict(m) | Error::Forbidden(m) => Some(&mut m.detail),
            _ => None,
        }
    }

    /// Name the field the error is about. The message is left as it is.
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        if let Some(detail) = self.detail_mut() {
            detail.field = Some(field.into());
        }
        self
    }

    /// Name the line of a document body the error is about, counted from
    /// 1. The message is left as it is.
    pub fn with_line(mut self, collection: impl Into<String>, line: usize) -> Self {
        if let Some(detail) = self.detail_mut() {
            detail.collection = Some(collection.into());
            detail.line = u32::try_from(line).ok();
        }
        self
    }

    /// Say why a conflict is one. The message is left as it is.
    pub fn with_reason(mut self, reason: Reason) -> Self {
        if let Error::Conflict(message) = &mut self {
            message.detail.reason = Some(reason);
        }
        self
    }

    /// The error as every transport sends it: `code` and `message`
    /// always, and `field`, `collection`, `line` and `reason` when known.
    /// The detail of an internal error is left out; the caller logs it.
    pub fn to_json(&self) -> serde_json::Value {
        let message = match self {
            Error::Internal(_) => "internal error".to_owned(),
            other => other.to_string(),
        };
        let mut body = serde_json::Map::new();
        body.insert("code".into(), self.code().into());
        body.insert("message".into(), message.into());
        if let Some(detail) = self.detail() {
            if let Some(field) = &detail.field {
                body.insert("field".into(), field.as_str().into());
            }
            if let Some(collection) = &detail.collection {
                body.insert("collection".into(), collection.as_str().into());
            }
            if let Some(line) = detail.line {
                body.insert("line".into(), line.into());
            }
            if let Some(reason) = detail.reason {
                body.insert("reason".into(), reason.as_str().into());
            }
        }
        serde_json::Value::Object(body)
    }
}

/// Database errors are classified by SQLSTATE so that constraint and trigger
/// failures reach the caller as validation errors, not as internal errors.
impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        if matches!(err, sqlx::Error::RowNotFound) {
            return Error::NotFound("record".into());
        }
        if let Some(db) = err.as_database_error() {
            let code = db.code().map(|c| c.into_owned()).unwrap_or_default();
            let message = db.message().to_string();
            return match code.as_str() {
                // unique_violation
                "23505" => Error::duplicate(match db.constraint() {
                    Some(c) => format!("a record with the same value already exists ({c})"),
                    None => "a record with the same value already exists".into(),
                }),
                // foreign_key_violation
                "23503" => Error::validation(match db.constraint() {
                    Some(c) => format!("a referenced record is missing or still in use ({c})"),
                    None => "a referenced record is missing or still in use".into(),
                }),
                // not_null, check, exclusion, raise, and bad input values
                "23502" | "23514" | "23P01" | "P0001" | "22P02" | "22007" | "22008" | "22003"
                | "22001" => Error::validation(message),
                // deadlock_detected, serialization_failure
                "40P01" | "40001" => Error::retry(
                    "another change to the same records was made at the same moment, and \
                     nothing was done: try again",
                ),
                _ => Error::Internal(message),
            };
        }
        Error::Internal(err.to_string())
    }
}

/// New time-ordered identifier.
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

/// Round an amount to a currency's minor units. Midpoints round away from
/// zero, the commercial convention, and the same rule Postgres `round()`
/// applies to `numeric`, so Rust and SQL always agree.
pub fn round_money(amount: Decimal, decimals: u32) -> Decimal {
    amount.round_dp_with_strategy(decimals, RoundingStrategy::MidpointAwayFromZero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn rounds_midpoints_away_from_zero() {
        let d = |s| Decimal::from_str(s).unwrap();
        assert_eq!(round_money(d("1.005"), 2), d("1.01"));
        assert_eq!(round_money(d("-1.005"), 2), d("-1.01"));
        assert_eq!(round_money(d("2.5"), 0), d("3"));
        assert_eq!(round_money(d("1.004"), 2), d("1.00"));
    }

    #[test]
    fn errors_keep_their_sentence_and_gain_detail() {
        let error = Error::field("rate", "has at most 10 decimal places");
        assert_eq!(
            error.to_string(),
            "field 'rate' has at most 10 decimal places"
        );
        assert_eq!(
            error.to_json(),
            serde_json::json!({
                "code": "validation",
                "message": "field 'rate' has at most 10 decimal places",
                "field": "rate",
            })
        );
        let error = Error::line(
            "lines",
            3,
            Some("qty"),
            "the quantity must be more than zero",
        );
        assert_eq!(
            error.to_json(),
            serde_json::json!({
                "code": "validation",
                "message": "line 3: the quantity must be more than zero",
                "collection": "lines",
                "line": 3,
                "field": "qty",
            })
        );
        assert_eq!(
            Error::stale("changed").to_json(),
            serde_json::json!({ "code": "conflict", "message": "changed", "reason": "stale" })
        );
        // A plain conflict has no reason, and the detail of an internal
        // error stays inside.
        assert_eq!(
            Error::conflict("busy").to_json(),
            serde_json::json!({ "code": "conflict", "message": "busy" })
        );
        assert_eq!(
            Error::internal("the disk is full").to_json(),
            serde_json::json!({ "code": "internal", "message": "internal error" })
        );
    }
}
