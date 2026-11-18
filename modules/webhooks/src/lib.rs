//! Webhooks module: tells HTTP endpoints outside about committed changes.
//!
//! It is an ordinary outbox subscriber, using only the public module API.
//! For every event it sends one signed POST to each active webhook that
//! wants the event's topic. A webhook that does not answer 2xx within ten
//! seconds makes the outbox retry the event later; the webhooks that
//! already have it are not sent it again.
//!
//! A webhook is created and changed only through this module's commands,
//! never through the generic entity commands. They are marked sensitive,
//! which keeps the signing secret, and a URL that may itself be a
//! credential, out of the audit log, the outbox and the idempotency table.

mod address;
mod delivery;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use erpcraft_kernel::entity::{DEFAULT_LIMIT, MAX_LIMIT, Page};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DynCommand, EntityMeta, Error, FieldMeta, FieldType, JsonSchema,
    Migration, Module, Result, Subscriber, async_trait, command, new_id, schemars,
};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub use address::{ALLOW_PRIVATE, check_url, is_public};
pub use delivery::{signature, wants};

const READ: &str = "webhook:read";
const CREATE: &str = "webhook:create";
const UPDATE: &str = "webhook:update";
const DELETE: &str = "webhook:delete";

/// The shortest secret accepted from a caller.
const MIN_SECRET: usize = 16;

pub struct WebhooksModule {
    /// Let webhooks point at private and local addresses.
    allow_private: bool,
}

impl WebhooksModule {
    /// Webhooks may point only at public addresses, unless the environment
    /// variable `ERPCRAFT_WEBHOOKS_ALLOW_PRIVATE` is `1`. It is read here,
    /// once, when the server starts.
    pub fn from_env() -> Self {
        WebhooksModule {
            allow_private: std::env::var(ALLOW_PRIVATE).is_ok_and(|value| value == "1"),
        }
    }

    /// For development and tests: say directly whether private and local
    /// addresses are allowed.
    pub fn allowing_private(allow_private: bool) -> Self {
        WebhooksModule { allow_private }
    }
}

impl Module for WebhooksModule {
    fn name(&self) -> &'static str {
        "webhooks"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "webhooks",
            sql: include_str!("../migrations/0001_webhooks.sql"),
        }]
    }

    /// Read-only: webhooks are written through this module's commands. The
    /// secret is not a field, and not a column of this table.
    fn entities(&self) -> Vec<EntityMeta> {
        let shown = |name: &str, label: &str, ty| FieldMeta::new(name, label, ty).readonly();
        vec![
            EntityMeta::new("webhook", "Webhook")
                .field(FieldMeta::new("name", "Name", FieldType::Text))
                .field(FieldMeta::new("url", "URL", FieldType::Text))
                .field(FieldMeta::new("topics", "Topics", FieldType::TextArray))
                .field(FieldMeta::new(
                    "company_id",
                    "Only for company",
                    FieldType::link("company"),
                ))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(shown("last_error", "Last error", FieldType::Text))
                .field(shown("last_status", "Last status", FieldType::Int))
                .field(shown("failed_at", "Failed at", FieldType::Timestamp))
                .field(shown("created_at", "Created", FieldType::Timestamp))
                .order_by(&["name"])
                .search(&["name", "url"])
                .read_only(),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        let allow_private = self.allow_private;
        vec![
            command(Create { allow_private }),
            command(Update { allow_private }),
            command(SetSecret),
            command(Delete),
            command(Deliveries),
        ]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        match delivery::Webhooks::new(self.allow_private) {
            Ok(webhooks) => vec![webhooks],
            Err(error) => {
                // Without a client nothing can be sent; say so rather than
                // take the server down with it.
                tracing::error!(%error, "webhooks are off: the HTTP client could not be set up");
                vec![]
            }
        }
    }
}

/// A webhook as the commands return it: everything but the secret.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
struct Webhook {
    id: Uuid,
    name: String,
    url: String,
    topics: Vec<String>,
    company_id: Option<Uuid>,
    active: bool,
    last_error: Option<String>,
    last_status: Option<i32>,
    failed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, name, url, topics, company_id, active, last_error, last_status, \
                       failed_at, created_at";

/// A secret nobody can guess: 244 random bits from the system's generator.
fn random_secret() -> String {
    format!(
        "whsec_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn check_secret(secret: &str) -> Result<()> {
    if secret.len() < MIN_SECRET {
        return Err(Error::validation(format!(
            "a secret has at least {MIN_SECRET} characters; leave it out to have one generated"
        )));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::validation("a webhook needs a name"));
    }
    Ok(name.to_owned())
}

/// Topics are command names, or a prefix ending in `*`. A name that no
/// command has would never match anything, so it is refused. The refusal
/// says which topic by its place in the list, not by repeating it: what
/// was sent may be something pasted into the wrong field.
fn check_topics(ctx: &Ctx<'_>, topics: &[String]) -> Result<Vec<String>> {
    let mut checked = Vec::with_capacity(topics.len());
    for (index, topic) in topics.iter().enumerate() {
        let n = index + 1;
        let topic = topic.trim();
        let name = topic.strip_suffix('*').unwrap_or(topic);
        let well_formed = name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.');
        if topic.is_empty() || !well_formed {
            return Err(Error::validation(format!(
                "topic {n} is not a topic: give a command name such as 'journal_entry.post', \
                 or a prefix ending in '*' such as 'purchase_order.*'"
            ))
            .with_field("topics"));
        }
        if name == topic && ctx.kernel.registry().command(topic).is_err() {
            return Err(Error::validation(format!(
                "topic {n} is not the name of a command, so no event has that topic"
            ))
            .with_field("topics"));
        }
        if !checked.iter().any(|t| t == topic) {
            checked.push(topic.to_owned());
        }
    }
    Ok(checked)
}

async fn check_company(ctx: &mut Ctx<'_>, company_id: Option<Uuid>) -> Result<()> {
    let Some(company_id) = company_id else {
        return Ok(());
    };
    let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM company WHERE id = $1)")
        .bind(company_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
    if known {
        Ok(())
    } else {
        Err(Error::validation("company_id must be a company"))
    }
}

/// A name can be used once. Said here, because the database's own message
/// would name a constraint.
fn name_taken(error: sqlx::Error) -> Error {
    match &error {
        sqlx::Error::Database(db) if db.constraint() == Some("webhook_name_key") => {
            Error::duplicate("a webhook with this name already exists")
        }
        _ => error.into(),
    }
}

// ------------------------------------------------------------------- create

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    /// What the webhook is for; unique among webhooks.
    name: String,
    /// Where events are sent. https, or http to localhost in development.
    url: String,
    /// Command names such as `journal_entry.post`, or a prefix ending in
    /// `*` such as `purchase_order.*`. Leave out or empty for every event.
    #[serde(default)]
    topics: Vec<String>,
    /// Send only events of this company. An event that names no company,
    /// such as one about a record shared by all companies or one of a
    /// command that handles secrets, goes only to webhooks without this.
    company_id: Option<Uuid>,
    /// The key requests are signed with, at least 16 characters. Leave out
    /// to have one generated.
    #[schemars(extend("writeOnly" = true))]
    secret: Option<String>,
    /// `true` when omitted.
    active: Option<bool>,
}

#[derive(Serialize, JsonSchema)]
struct Created {
    #[serde(flatten)]
    webhook: Webhook,
    /// Shown this once. It cannot be read back, only replaced.
    secret: String,
}

struct Create {
    allow_private: bool,
}

#[async_trait]
impl Command for Create {
    type Input = CreateInput;
    type Output = Created;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "webhook.create",
            "Create a webhook: an HTTP endpoint that is sent a signed POST for every committed \
             change it subscribes to. The body is {id, topic, created_at, payload}; the header \
             X-Erpcraft-Signature is 'sha256=' and the hex HMAC-SHA256 of the body under the \
             webhook's secret. The secret is returned once, here, and never again.",
        )
        .permission(CREATE)
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CreateInput) -> Result<Created> {
        ctx.actor.require_global(CREATE)?;
        let name = check_name(&input.name)?;
        let url = address::check_target(&input.url, self.allow_private).await?;
        let topics = check_topics(ctx, &input.topics)?;
        check_company(ctx, input.company_id).await?;
        let secret = match input.secret {
            Some(secret) => {
                check_secret(&secret)?;
                secret
            }
            None => random_secret(),
        };

        let id = new_id();
        ctx.touch("webhook", id);
        let webhook: Webhook = sqlx::query_as(&format!(
            "INSERT INTO webhook (id, name, url, topics, company_id, active)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
        ))
        .bind(id)
        .bind(&name)
        .bind(url.as_str())
        .bind(&topics)
        .bind(input.company_id)
        .bind(input.active.unwrap_or(true))
        .fetch_one(&mut *ctx.conn)
        .await
        .map_err(name_taken)?;
        sqlx::query("INSERT INTO webhook_secret (webhook_id, secret) VALUES ($1, $2)")
            .bind(id)
            .bind(&secret)
            .execute(&mut *ctx.conn)
            .await?;
        Ok(Created { webhook, secret })
    }
}

// ------------------------------------------------------------------- update

/// Tells a field that was left out from one that was sent as null.
fn sent<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateInput {
    /// Webhook id.
    id: Uuid,
    /// A new name. Unchanged when omitted.
    name: Option<String>,
    /// A new address to send events to. Unchanged when omitted.
    url: Option<String>,
    /// Replaces the topics. Empty for every event.
    topics: Option<Vec<String>>,
    /// Send only events of this company; null for every company.
    #[serde(default, deserialize_with = "sent")]
    company_id: Option<Option<Uuid>>,
    /// `false` stops deliveries without deleting the webhook.
    active: Option<bool>,
}

struct Update {
    allow_private: bool,
}

#[async_trait]
impl Command for Update {
    type Input = UpdateInput;
    type Output = Webhook;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "webhook.update",
            "Change a webhook's name, URL, topics, company or whether it is active. Fields \
             left out stay as they are. The secret is changed with webhook.set_secret. An \
             inactive webhook is sent nothing, and events that pass while it is inactive are \
             not sent later.",
        )
        .permission(UPDATE)
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UpdateInput) -> Result<Webhook> {
        ctx.actor.require_global(UPDATE)?;
        ctx.touch("webhook", input.id);
        let current: Webhook = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM webhook WHERE id = $1 FOR UPDATE"
        ))
        .bind(input.id)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found("webhook"))?;

        let name = match &input.name {
            Some(name) => check_name(name)?,
            None => current.name,
        };
        let url = match &input.url {
            Some(url) => address::check_target(url, self.allow_private)
                .await?
                .to_string(),
            None => current.url,
        };
        let topics = match &input.topics {
            Some(topics) => check_topics(ctx, topics)?,
            None => current.topics,
        };
        let company_id = match input.company_id {
            Some(company_id) => {
                check_company(ctx, company_id).await?;
                company_id
            }
            None => current.company_id,
        };

        let webhook = sqlx::query_as(&format!(
            "UPDATE webhook SET name = $2, url = $3, topics = $4, company_id = $5, active = $6
             WHERE id = $1 RETURNING {COLUMNS}"
        ))
        .bind(input.id)
        .bind(&name)
        .bind(&url)
        .bind(&topics)
        .bind(company_id)
        .bind(input.active.unwrap_or(current.active))
        .fetch_one(&mut *ctx.conn)
        .await
        .map_err(name_taken)?;
        Ok(webhook)
    }
}

// --------------------------------------------------------------- set secret

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetSecretInput {
    /// Webhook id.
    id: Uuid,
    /// The new key, at least 16 characters. Leave out to have one
    /// generated.
    #[schemars(extend("writeOnly" = true))]
    secret: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct SecretSet {
    id: Uuid,
    /// The new secret if it was generated here, shown this once. A secret
    /// the caller sent is not sent back.
    secret: Option<String>,
}

struct SetSecret;

#[async_trait]
impl Command for SetSecret {
    type Input = SetSecretInput;
    type Output = SecretSet;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "webhook.set_secret",
            "Replace the secret a webhook's requests are signed with. Requests sent from now \
             on are signed with the new one. A generated secret is returned once.",
        )
        .permission(UPDATE)
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetSecretInput) -> Result<SecretSet> {
        ctx.actor.require_global(UPDATE)?;
        ctx.touch("webhook", input.id);
        let generated = input.secret.is_none();
        let secret = match input.secret {
            Some(secret) => {
                check_secret(&secret)?;
                secret
            }
            None => random_secret(),
        };
        let updated = sqlx::query("UPDATE webhook_secret SET secret = $2 WHERE webhook_id = $1")
            .bind(input.id)
            .bind(&secret)
            .execute(&mut *ctx.conn)
            .await?
            .rows_affected();
        if updated == 0 {
            return Err(Error::not_found("webhook"));
        }
        Ok(SecretSet {
            id: input.id,
            secret: generated.then_some(secret),
        })
    }
}

// ------------------------------------------------------------------- delete

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdInput {
    /// Webhook id.
    id: Uuid,
}

#[derive(Serialize, JsonSchema)]
struct Deleted {
    id: Uuid,
}

struct Delete;

#[async_trait]
impl Command for Delete {
    type Input = IdInput;
    type Output = Deleted;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "webhook.delete",
            "Delete a webhook, with its secret and the record of what was sent to it.",
        )
        .destructive()
        .permission(DELETE)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<Deleted> {
        ctx.actor.require_global(DELETE)?;
        ctx.touch("webhook", input.id);
        let deleted = sqlx::query("DELETE FROM webhook WHERE id = $1")
            .bind(input.id)
            .execute(&mut *ctx.conn)
            .await?
            .rows_affected();
        if deleted == 0 {
            return Err(Error::not_found("webhook"));
        }
        Ok(Deleted { id: input.id })
    }
}

// --------------------------------------------------------------- deliveries

#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    /// Both.
    #[default]
    All,
    /// Sent and answered with 2xx.
    Delivered,
    /// Tried and not delivered so far.
    Failed,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeliveriesInput {
    /// Webhook id.
    webhook_id: Uuid,
    /// `all` when omitted.
    #[serde(default)]
    outcome: Outcome,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Deliveries to skip, for paging.
    offset: Option<i64>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct Delivery {
    /// The outbox event; also the `X-Erpcraft-Delivery` header.
    event_id: Uuid,
    topic: String,
    /// How many times the event was sent to this webhook.
    attempts: i32,
    /// What the endpoint answered at the last attempt, if it answered.
    status_code: Option<i32>,
    /// Why the last attempt failed.
    error: Option<String>,
    attempted_at: DateTime<Utc>,
    /// When it arrived. Null while it is failing.
    delivered_at: Option<DateTime<Utc>>,
}

struct Deliveries;

#[async_trait]
impl Command for Deliveries {
    type Input = DeliveriesInput;
    type Output = Page<Delivery>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "webhook.deliveries",
            "What was sent to one webhook, latest attempt first: each event with the number \
             of attempts, the endpoint's answer, and the error if it has not arrived. Kept as \
             long as the events themselves, 30 days after they are delivered.",
        )
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DeliveriesInput) -> Result<Page<Delivery>> {
        ctx.actor.require_global(READ)?;
        let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM webhook WHERE id = $1)")
            .bind(input.webhook_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        if !known {
            return Err(Error::not_found("webhook"));
        }
        let filter = match input.outcome {
            Outcome::All => "true",
            Outcome::Delivered => "delivered_at IS NOT NULL",
            Outcome::Failed => "delivered_at IS NULL",
        };
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM webhook_delivery WHERE webhook_id = $1 AND {filter}"
        ))
        .bind(input.webhook_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let items = sqlx::query_as(&format!(
            "SELECT event_id, topic, attempts, status_code, error, attempted_at, delivered_at
             FROM webhook_delivery WHERE webhook_id = $1 AND {filter}
             ORDER BY attempted_at DESC, event_id DESC LIMIT $2 OFFSET $3"
        ))
        .bind(input.webhook_id)
        .bind(input.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
        .bind(input.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(Page { items, total })
    }
}
