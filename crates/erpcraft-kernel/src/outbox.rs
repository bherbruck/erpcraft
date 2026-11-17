//! Delivering outbox events to subscribers.
//!
//! Every command that changes something writes an event in its own
//! transaction, so the event exists if and only if the change committed.
//! Delivery happens afterwards, here.
//!
//! What a subscriber can rely on, and what it cannot:
//!
//! - **At least once.** An event is retried until every subscriber that
//!   wants it has handled it. A subscriber that succeeded is not called
//!   again for that event, except when the server stops between the
//!   handling and the note that it happened. Handlers must tolerate a
//!   repeat.
//! - **No order.** Events are picked up roughly oldest first, but a failing
//!   event does not hold back the ones behind it.
//! - **After commit.** A handler never sees an event whose change was
//!   rolled back.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use erpcraft_core::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::Kernel;
use crate::command::{Command, CommandSpec, Ctx, DynCommand, command};
use crate::entity::{DEFAULT_LIMIT, MAX_LIMIT, Page};

/// The permission to look at the outbox. Checked for all companies.
pub const READ: &str = "outbox:read";

/// A committed change, as written by the command executor.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Event {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    /// The name of the command that made the change.
    pub topic: String,
    /// `command`, `user_id` and, unless the command handles secrets,
    /// `input` and `output`.
    pub payload: Value,
}

/// Something that reacts to committed changes: an integration, a
/// notification, a projection. Modules supply subscribers through
/// [`Module::subscribers`](crate::Module::subscribers).
#[async_trait]
pub trait Subscriber: Send + Sync + 'static {
    /// Unique lowercase identifier. It records which events this subscriber
    /// has handled, so it must never change once released.
    fn name(&self) -> &'static str;

    /// Whether this subscriber wants events of `topic`, a command name.
    fn wants(&self, topic: &str) -> bool;

    /// Handle one event. An error leaves the event to be retried later,
    /// with a growing delay.
    async fn handle(&self, kernel: &Kernel, event: &Event) -> Result<()>;
}

/// What one pass over the outbox did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Delivered {
    /// Events now handled by every subscriber that wants them.
    pub published: usize,
    /// Events that failed and will be retried.
    pub failed: usize,
}

impl Delivered {
    pub fn total(&self) -> usize {
        self.published + self.failed
    }
}

/// Deliver up to `limit` due events. Safe to call from several servers at
/// once: each event is taken by one of them.
pub async fn deliver_batch(kernel: &Kernel, limit: i64) -> Result<Delivered> {
    let mut tx = kernel.pool().begin().await?;
    // NO KEY UPDATE, not UPDATE: the events stay locked against other
    // workers for the whole pass, while deliveries, which reference them,
    // are noted on other connections.
    let events: Vec<Event> = sqlx::query_as(
        "SELECT id, created_at, topic, payload FROM outbox_event
         WHERE published_at IS NULL AND next_attempt_at <= now()
         ORDER BY id LIMIT $1 FOR NO KEY UPDATE SKIP LOCKED",
    )
    .bind(limit)
    .fetch_all(&mut *tx)
    .await?;

    let mut delivered = Delivered::default();
    for event in &events {
        match deliver(kernel, event).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE outbox_event SET published_at = now(), last_error = NULL WHERE id = $1",
                )
                .bind(event.id)
                .execute(&mut *tx)
                .await?;
                delivered.published += 1;
            }
            Err(error) => {
                tracing::warn!(event = %event.id, topic = %event.topic, %error, "outbox delivery failed");
                // 10 seconds, doubling, at most an hour, counted from now
                // and not from when this pass began.
                sqlx::query(
                    "UPDATE outbox_event SET
                        attempts = attempts + 1,
                        last_error = $2,
                        next_attempt_at = clock_timestamp()
                            + make_interval(secs => LEAST(3600, 10 * power(2, LEAST(attempts, 10))))
                     WHERE id = $1",
                )
                .bind(event.id)
                .bind(&error)
                .execute(&mut *tx)
                .await?;
                delivered.failed += 1;
            }
        }
    }
    tx.commit().await?;
    Ok(delivered)
}

/// Hand one event to every subscriber that wants it and has not had it.
/// Deliveries are noted outside the batch's transaction, so they stand even
/// if a later subscriber fails.
async fn deliver(kernel: &Kernel, event: &Event) -> std::result::Result<(), String> {
    let subscribers: Vec<&Arc<dyn Subscriber>> = kernel
        .registry()
        .subscribers()
        .iter()
        .filter(|s| s.wants(&event.topic))
        .collect();
    if subscribers.is_empty() {
        return Ok(());
    }
    let done: Vec<String> =
        sqlx::query_scalar("SELECT subscriber FROM outbox_delivery WHERE event_id = $1")
            .bind(event.id)
            .fetch_all(kernel.pool())
            .await
            .map_err(|e| e.to_string())?;
    for subscriber in subscribers {
        let name = subscriber.name();
        if done.iter().any(|d| d == name) {
            continue;
        }
        subscriber
            .handle(kernel, event)
            .await
            .map_err(|e| format!("{name}: {e}"))?;
        sqlx::query(
            "INSERT INTO outbox_delivery (event_id, subscriber) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(event.id)
        .bind(name)
        .execute(kernel.pool())
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// An event with the state of its delivery.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EventStatus {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub topic: String,
    pub published_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub next_attempt_at: DateTime<Utc>,
}

pub(crate) fn commands() -> Vec<Arc<dyn DynCommand>> {
    vec![command(List)]
}

#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum State {
    /// Not yet handled by every subscriber.
    Pending,
    /// Pending, and at least one delivery has failed.
    #[default]
    Failing,
    Published,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// `failing` when omitted.
    #[serde(default)]
    state: State,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    offset: Option<i64>,
}

struct List;

#[async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = Page<EventStatus>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "outbox.list",
            "Events waiting to be delivered to subscribers, oldest first. By default only those \
             whose delivery has failed, with the last error.",
        )
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ListInput) -> Result<Page<EventStatus>> {
        ctx.actor.require_global(READ)?;
        let filter = match input.state {
            State::Pending => "published_at IS NULL",
            State::Failing => "published_at IS NULL AND attempts > 0",
            State::Published => "published_at IS NOT NULL",
        };
        let total: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM outbox_event WHERE {filter}"))
                .fetch_one(&mut *ctx.conn)
                .await?;
        let items = sqlx::query_as(&format!(
            "SELECT id, created_at, topic, published_at, attempts, last_error, next_attempt_at
             FROM outbox_event WHERE {filter} ORDER BY id LIMIT $1 OFFSET $2"
        ))
        .bind(input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
        .bind(input.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(Page { items, total })
    }
}
