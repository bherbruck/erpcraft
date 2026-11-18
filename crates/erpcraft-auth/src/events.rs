//! Sign-ins and refused sign-ins, kept for an administrator to look at.
//!
//! The audit log records commands, and signing in is not one: the caller
//! is nobody yet. So sign-ins have a small log of their own. It never
//! holds a password, token, code, state or client secret.
//!
//! Changes to single sign-on providers and to the identities users are
//! bound to are kept here too. They decide who can sign in as whom, and
//! the commands that make them are partly sensitive, so the audit log
//! does not show what they changed.

use chrono::{DateTime, Utc};
use erpcraft_kernel::entity::Page;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, JsonSchema, PgConnection, PgPool, Result, async_trait, new_id,
    schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const READ: &str = "auth_event:read";
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 500;
/// Addresses and reasons come from outside; what is stored is bounded.
const MAX_TEXT: usize = 320;

/// One sign-in, successful or refused, about to be recorded.
#[derive(Default)]
pub(crate) struct AuthEvent<'a> {
    /// `password` or `sso`.
    pub kind: &'static str,
    pub email: Option<&'a str>,
    /// The provider's slug.
    pub provider: Option<&'a str>,
    /// Who the provider says signed in (`sub`).
    pub subject: Option<&'a str>,
    pub user_id: Option<Uuid>,
    /// `None` for a success, the reason for a refusal.
    pub refused: Option<&'a str>,
}

fn bounded(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

/// Record a sign-in. A failure to record is logged and does not change
/// the outcome of the sign-in.
pub(crate) async fn record(pool: &PgPool, event: AuthEvent<'_>) {
    let result = sqlx::query(
        "INSERT INTO auth_event (id, kind, email, provider, subject, user_id, outcome, reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(new_id())
    .bind(event.kind)
    .bind(event.email.map(bounded))
    .bind(event.provider.map(bounded))
    .bind(event.subject.map(bounded))
    .bind(event.user_id)
    .bind(if event.refused.is_some() {
        "refused"
    } else {
        "success"
    })
    .bind(event.refused.map(bounded))
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::error!(%error, "a sign-in could not be recorded");
    }
}

/// Record, in the transaction of the command that makes it, a change to a
/// provider (`sso_provider`) or to the identity a user is bound to
/// (`sso_identity`): who made it, to which provider, and `what` changed.
/// Unlike a sign-in, the change does not happen if it cannot be recorded.
pub(crate) async fn record_change(
    ctx: &mut Ctx<'_>,
    kind: &'static str,
    provider: &str,
    what: &str,
) -> Result<()> {
    record_on(
        &mut *ctx.conn,
        kind,
        &ctx.actor.email,
        ctx.actor.user_id,
        provider,
        what,
    )
    .await
}

pub(crate) async fn record_on(
    conn: &mut PgConnection,
    kind: &'static str,
    email: &str,
    user_id: Uuid,
    provider: &str,
    what: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO auth_event (id, kind, email, provider, user_id, outcome, reason)
         VALUES ($1, $2, $3, $4, $5, 'success', $6)",
    )
    .bind(new_id())
    .bind(kind)
    .bind(bounded(email))
    .bind(bounded(provider))
    .bind(user_id)
    .bind(bounded(what))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    #[default]
    All,
    Success,
    Refused,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventsInput {
    /// `success`, `refused`, or `all` (the default).
    #[serde(default)]
    outcome: Outcome,
    /// Only sign-ins with exactly this email address.
    email: Option<String>,
    /// Only events of the provider with this slug.
    provider: Option<String>,
    /// Only events of this kind: `password` and `sso` are sign-ins,
    /// `sso_provider` is a change to a provider's settings or secret, and
    /// `sso_identity` a change to the identity a user is bound to.
    kind: Option<String>,
    /// Events per page: 50 by default, 500 at most.
    limit: Option<i64>,
    /// Events to skip, for paging.
    offset: Option<i64>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct Event {
    id: Uuid,
    at: DateTime<Utc>,
    /// `password` or `sso` for a sign-in; `sso_provider` for a change to
    /// a provider, `sso_identity` for a change to a user's binding.
    kind: String,
    /// For a sign-in, the address given or stated by the provider. For a
    /// change, the address of who made it.
    email: Option<String>,
    /// The slug of the single sign-on provider.
    provider: Option<String>,
    /// Who the provider said was signing in (`sub`). This is what
    /// sso_identity.set binds a user to.
    subject: Option<String>,
    /// For a sign-in, the user, when there is one. For a change, who made it.
    user_id: Option<Uuid>,
    /// `success` or `refused`.
    outcome: String,
    /// Why a sign-in was refused, or what a change changed.
    reason: Option<String>,
}

pub(crate) struct Events;

#[async_trait]
impl Command for Events {
    type Input = EventsInput;
    type Output = Page<Event>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "auth.events",
            "Sign-ins and refused sign-ins, latest first: when, by password or through which \
             single sign-on provider, the email address, the identity the provider stated, the \
             user if there is one, and why a sign-in was refused. Also every change to a \
             single sign-on provider and to the identity a user is bound to: who made it and \
             what changed. Returns the page as `items` and the size of the whole list as \
             `total`.",
        )
        .permission(READ)
        .label("Sign-ins")
        .report("Sign-ins")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EventsInput) -> Result<Page<Event>> {
        ctx.actor.require_global(READ)?;
        let outcome = match input.outcome {
            Outcome::All => None,
            Outcome::Success => Some("success"),
            Outcome::Refused => Some("refused"),
        };
        let email = input.email.map(|email| email.trim().to_lowercase());
        const FILTER: &str = "($1::text IS NULL OR outcome = $1)
             AND ($2::text IS NULL OR lower(email) = $2)
             AND ($3::text IS NULL OR provider = $3)
             AND ($4::text IS NULL OR kind = $4)";
        let total: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM auth_event WHERE {FILTER}"))
                .bind(outcome)
                .bind(&email)
                .bind(&input.provider)
                .bind(&input.kind)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let items = sqlx::query_as(&format!(
            "SELECT id, at, kind, email, provider, subject, user_id, outcome, reason
             FROM auth_event WHERE {FILTER}
             ORDER BY at DESC, id DESC LIMIT $5 OFFSET $6"
        ))
        .bind(outcome)
        .bind(&email)
        .bind(&input.provider)
        .bind(&input.kind)
        .bind(input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
        .bind(input.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(Page { items, total })
    }
}
