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
    Validation(String),
    #[error("{0} not found")]
    NotFound(String),
    /// The request conflicts with the current state of the record.
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("not authenticated")]
    Unauthorized,
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Error::Forbidden(msg.into())
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
                "23505" => Error::Conflict(match db.constraint() {
                    Some(c) => format!("a record with the same value already exists ({c})"),
                    None => "a record with the same value already exists".into(),
                }),
                // foreign_key_violation
                "23503" => Error::Validation(match db.constraint() {
                    Some(c) => format!("a referenced record is missing or still in use ({c})"),
                    None => "a referenced record is missing or still in use".into(),
                }),
                // not_null, check, exclusion, raise, and bad input values
                "23502" | "23514" | "23P01" | "P0001" | "22P02" | "22007" | "22008" | "22003"
                | "22001" => Error::Validation(message),
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
}
