//! Reading the audit log.
//!
//! The log is written by the command executor: one entry per command that
//! changed something or tried to, with the records it touched. Nothing here
//! writes to it.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use erpcraft_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::command::{Command, CommandSpec, Ctx, DynCommand, command};
use crate::document;
use crate::entity::{self, DEFAULT_LIMIT, MAX_LIMIT, Page};

/// The permission to read the audit log itself, including what each
/// command was given as input. It is only ever checked for all companies.
pub const READ: &str = "audit:read";

#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub user_id: Uuid,
    pub user_name: String,
    pub command: String,
    /// `ok` or `error`.
    pub outcome: String,
    pub error_code: Option<String>,
    /// What the caller was told. It can quote the input, so it is absent
    /// for callers without `audit:read`, like the input.
    pub error_message: Option<String>,
    /// What the command was given. Absent for commands that handle secrets,
    /// and for callers without `audit:read`.
    pub input: Option<Value>,
}

const COLUMNS: &str = "a.id, a.at, a.user_id, u.name AS user_name, a.command, a.outcome, \
                       a.error_code";

fn page(limit: Option<i64>, offset: Option<i64>) -> (i64, i64) {
    (
        limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        offset.unwrap_or(0).max(0),
    )
}

/// The commands that touched one record, newest first.
///
/// Anyone who may read the record may read who did what to it and when.
/// The inputs of those commands can hold fields the caller may not see, so
/// they are included only for callers with `audit:read`.
pub async fn history(
    ctx: &mut Ctx<'_>,
    resource: &str,
    id: Uuid,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Page<AuditEntry>> {
    let registry = ctx.kernel.registry();
    let readable = if registry.entity(resource).is_ok() {
        entity::get(ctx, resource, id).await.map(drop)
    } else if registry.doc_type(resource).is_ok() {
        document::header(ctx, resource, id).await.map(drop)
    } else {
        ctx.actor.require_global(READ)
    };
    match readable {
        // A deleted record still has a history, for those who may read the log.
        Err(Error::NotFound(_)) if ctx.actor.can_global(READ) => {}
        other => other?,
    }
    let with_input = ctx.actor.can_global(READ);
    let (limit, offset) = page(limit, offset);

    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_subject WHERE resource = $1 AND record_id = $2",
    )
    .bind(resource)
    .bind(id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    let items = sqlx::query_as(&format!(
        "SELECT {COLUMNS}, CASE WHEN $3 THEN a.error_message END AS error_message,
                CASE WHEN $3 THEN a.input END AS input
         FROM audit_subject s
         JOIN audit_log a ON a.id = s.audit_id
         JOIN app_user u ON u.id = a.user_id
         WHERE s.resource = $1 AND s.record_id = $2
         ORDER BY s.audit_id DESC LIMIT $4 OFFSET $5"
    ))
    .bind(resource)
    .bind(id)
    .bind(with_input)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(Page { items, total })
}

/// What to look for in the whole log.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub user_id: Option<Uuid>,
    pub command: Option<String>,
    /// `ok` or `error`.
    pub outcome: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// The audit log, newest first. Requires `audit:read` for all companies.
pub async fn list(ctx: &mut Ctx<'_>, query: &AuditQuery) -> Result<Page<AuditEntry>> {
    ctx.actor.require_global(READ)?;
    let (limit, offset) = page(query.limit, query.offset);
    const WHERE: &str = "($1::uuid IS NULL OR a.user_id = $1)
        AND ($2::text IS NULL OR a.command = $2)
        AND ($3::text IS NULL OR a.outcome = $3)";

    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM audit_log a WHERE {WHERE}"))
        .bind(query.user_id)
        .bind(&query.command)
        .bind(&query.outcome)
        .fetch_one(&mut *ctx.conn)
        .await?;
    let items = sqlx::query_as(&format!(
        "SELECT {COLUMNS}, a.error_message, a.input
         FROM audit_log a JOIN app_user u ON u.id = a.user_id
         WHERE {WHERE}
         ORDER BY a.id DESC LIMIT $4 OFFSET $5"
    ))
    .bind(query.user_id)
    .bind(&query.command)
    .bind(&query.outcome)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(Page { items, total })
}

pub(crate) fn commands() -> Vec<Arc<dyn DynCommand>> {
    vec![command(History), command(List)]
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryInput {
    /// An entity name, a document type, or `user`.
    resource: String,
    /// Id of the record or document.
    id: Uuid,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Records to skip, for paging.
    offset: Option<i64>,
}

struct History;

#[async_trait]
impl Command for History {
    type Input = HistoryInput;
    type Output = Page<AuditEntry>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "audit.history",
            "The commands that created, changed or acted on one record, newest first, including \
             attempts that failed. Open to anyone who may read the record; the inputs of the \
             commands are included only with the permission `audit:read`.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: HistoryInput) -> Result<Page<AuditEntry>> {
        history(ctx, &input.resource, input.id, input.limit, input.offset).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Ok,
    Error,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// Only commands run by this user.
    user_id: Option<Uuid>,
    /// Only this command, for example `journal_entry.post`.
    command: Option<String>,
    /// Only commands that succeeded, or only those that failed.
    outcome: Option<Outcome>,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Records to skip, for paging.
    offset: Option<i64>,
}

struct List;

#[async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = Page<AuditEntry>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "audit.list",
            "The audit log, newest first: every command that changed something or tried to, \
             with who ran it, when, and its input.",
        )
        .report("Audit log")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ListInput) -> Result<Page<AuditEntry>> {
        let query = AuditQuery {
            user_id: input.user_id,
            command: input.command,
            outcome: input.outcome.map(|outcome| match outcome {
                Outcome::Ok => "ok".to_owned(),
                Outcome::Error => "error".to_owned(),
            }),
            limit: input.limit,
            offset: input.offset,
        };
        list(ctx, &query).await
    }
}
