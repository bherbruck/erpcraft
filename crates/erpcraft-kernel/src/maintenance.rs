//! Removing what is only useful for a while.
//!
//! Neither table cleaned here is a record of the business: the audit log
//! and the ledgers are, and they are never cleaned.

use erpcraft_core::Result;

use crate::Kernel;

/// How long to keep things, in hours.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    /// Idempotency keys. A retry with a key older than this runs the
    /// command again, so this must exceed the longest time a caller keeps
    /// retrying.
    pub idempotency_hours: i32,
    /// Events every subscriber has handled.
    pub published_event_hours: i32,
}

impl Default for Retention {
    /// One day for keys, thirty days for delivered events.
    fn default() -> Self {
        Retention {
            idempotency_hours: 24,
            published_event_hours: 24 * 30,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub idempotency_keys: u64,
    pub events: u64,
}

/// Delete expired idempotency keys and old delivered events.
pub async fn clean(kernel: &Kernel, retention: Retention) -> Result<Cleaned> {
    let idempotency_keys = sqlx::query(
        "DELETE FROM idempotency_key WHERE created_at < now() - make_interval(hours => $1)",
    )
    .bind(retention.idempotency_hours)
    .execute(kernel.pool())
    .await?
    .rows_affected();
    let events = sqlx::query(
        "DELETE FROM outbox_event
         WHERE published_at IS NOT NULL AND published_at < now() - make_interval(hours => $1)",
    )
    .bind(retention.published_event_hours)
    .execute(kernel.pool())
    .await?
    .rows_affected();
    Ok(Cleaned {
        idempotency_keys,
        events,
    })
}
