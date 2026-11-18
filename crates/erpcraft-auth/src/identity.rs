//! The identity at a provider that a user is bound to.
//!
//! A user's first sign-in through a provider binds them to the provider
//! and the subject (`sub`) it states; from then on the user is accepted
//! only from that provider under that subject. These commands are the
//! explicit way to change that: bind a user to another identity, or undo
//! the binding so that the next allowed sign-in binds them again.

use chrono::{DateTime, Utc};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::events;
use crate::sso::{MANAGE, READ};

const MAX_SUBJECT: usize = 255;

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct Binding {
    user_id: Uuid,
    /// The user's email address here.
    email: String,
    provider_id: Uuid,
    /// The provider's slug.
    provider: String,
    /// Who the user is at the provider (`sub`).
    subject: String,
    created_at: DateTime<Utc>,
}

/// The user, locked, with whether they are an administrator. Changing the
/// binding of an administrator decides who can sign in as one, so only an
/// administrator can.
async fn target(ctx: &mut Ctx<'_>, user_id: Uuid) -> Result<String> {
    let user: Option<(String, bool)> =
        sqlx::query_as("SELECT email, is_admin FROM app_user WHERE id = $1 FOR UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    let Some((email, is_admin)) = user else {
        return Err(Error::field("user_id", "must be a user"));
    };
    if is_admin && !ctx.actor.is_admin {
        return Err(Error::forbidden(
            "only an administrator can change how an administrator signs in",
        ));
    }
    Ok(email)
}

async fn current(ctx: &mut Ctx<'_>, user_id: Uuid) -> Result<Option<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT p.slug, i.subject FROM sso_identity i JOIN sso_provider p ON p.id = i.provider_id
         WHERE i.user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *ctx.conn)
    .await?)
}

// ----------------------------------------------------------------------- set

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetInput {
    /// The user to bind.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "user", "key": "id" }))]
    user_id: Uuid,
    /// The provider the user is to sign in through.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "sso_provider", "key": "id" }))]
    provider_id: Uuid,
    /// Who the user is at that provider: the subject (`sub`) it states.
    /// auth.events shows the subject of every sign-in, refused ones too.
    subject: String,
}

pub(crate) struct Set;

#[async_trait]
impl Command for Set {
    type Input = SetInput;
    type Output = Binding;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sso_identity.set",
            "Bind a user to an identity at a single sign-on provider, replacing the one they \
             are bound to. From then on the user can sign in through a provider only from this \
             one, as this subject (`sub`). Takes the permission sso_provider:manage for all \
             companies; binding an administrator takes an administrator. Recorded in \
             auth.events.",
        )
        .permission(MANAGE)
        .label("Bind to identity")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetInput) -> Result<Binding> {
        ctx.actor.require_global(MANAGE)?;
        let subject = input.subject.trim().to_owned();
        if subject.is_empty()
            || subject.chars().count() > MAX_SUBJECT
            || subject.chars().any(char::is_control)
        {
            return Err(Error::field(
                "subject",
                format!("must have 1 to {MAX_SUBJECT} characters"),
            ));
        }
        ctx.touch("user", input.user_id);
        let email = target(ctx, input.user_id).await?;
        let slug: Option<String> =
            sqlx::query_scalar("SELECT slug FROM sso_provider WHERE id = $1")
                .bind(input.provider_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        let Some(slug) = slug else {
            return Err(Error::field("provider_id", "must be a provider"));
        };
        let before = current(ctx, input.user_id).await?;
        let taken: Option<String> = sqlx::query_scalar(
            "SELECT u.email FROM sso_identity i JOIN app_user u ON u.id = i.user_id
             WHERE i.provider_id = $1 AND i.subject = $2 AND i.user_id <> $3",
        )
        .bind(input.provider_id)
        .bind(&subject)
        .bind(input.user_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some(other) = taken {
            return Err(Error::duplicate(format!(
                "this identity is bound to {other}; undo that with sso_identity.delete first"
            ))
            .with_field("subject"));
        }
        let binding: Binding = sqlx::query_as(
            "WITH b AS (
                INSERT INTO sso_identity (user_id, provider_id, subject) VALUES ($1, $2, $3)
                ON CONFLICT (user_id) DO UPDATE
                    SET provider_id = EXCLUDED.provider_id, subject = EXCLUDED.subject,
                        created_at = now()
                RETURNING user_id, provider_id, subject, created_at)
             SELECT b.user_id, $4::text AS email, b.provider_id, $5::text AS provider, b.subject,
                    b.created_at
             FROM b",
        )
        .bind(input.user_id)
        .bind(input.provider_id)
        .bind(&subject)
        .bind(&email)
        .bind(&slug)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let was = match before {
            Some((slug, subject)) => format!(", was subject {subject} at {slug}"),
            None => ", was not bound".to_owned(),
        };
        events::record_change(
            ctx,
            "sso_identity",
            &slug,
            &format!("bound {email} to subject {subject}{was}"),
        )
        .await?;
        Ok(binding)
    }
}

// -------------------------------------------------------------------- delete

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteInput {
    /// The user whose binding is undone.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "user", "key": "id" }))]
    user_id: Uuid,
}

pub(crate) struct Delete;

#[async_trait]
impl Command for Delete {
    type Input = DeleteInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sso_identity.delete",
            "Undo a user's binding to an identity at a single sign-on provider. The next \
             sign-in through a provider that names the domain of the user's email address binds \
             them again. Tokens issued before stay valid. Takes the permission \
             sso_provider:manage for all companies; for an administrator, an administrator. \
             Recorded in auth.events.",
        )
        .permission(MANAGE)
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DeleteInput) -> Result<Value> {
        ctx.actor.require_global(MANAGE)?;
        ctx.touch("user", input.user_id);
        let email = target(ctx, input.user_id).await?;
        let Some((slug, subject)) = current(ctx, input.user_id).await? else {
            return Err(Error::not_found("sso_identity"));
        };
        sqlx::query("DELETE FROM sso_identity WHERE user_id = $1")
            .bind(input.user_id)
            .execute(&mut *ctx.conn)
            .await?;
        events::record_change(
            ctx,
            "sso_identity",
            &slug,
            &format!("unbound {email} from subject {subject}"),
        )
        .await?;
        Ok(json!({ "user_id": input.user_id, "deleted": true }))
    }
}

// ---------------------------------------------------------------------- list

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListInput {
    /// Only the binding of this user.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "user", "key": "id" }))]
    user_id: Option<Uuid>,
    /// Only the users bound to this provider.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "sso_provider", "key": "id" }))]
    provider_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ListOutput {
    /// The bindings, by email address.
    items: Vec<Binding>,
}

pub(crate) struct List;

#[async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = ListOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "sso_identity.list",
            "Which users are bound to which identity at which single sign-on provider. A user \
             who is bound signs in through a provider only from that one, as that subject.",
        )
        .permission(READ)
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ListInput) -> Result<ListOutput> {
        ctx.actor.require_global(READ)?;
        let items = sqlx::query_as(
            "SELECT i.user_id, u.email, i.provider_id, p.slug AS provider, i.subject, i.created_at
             FROM sso_identity i
             JOIN app_user u ON u.id = i.user_id
             JOIN sso_provider p ON p.id = i.provider_id
             WHERE ($1::uuid IS NULL OR i.user_id = $1) AND ($2::uuid IS NULL OR i.provider_id = $2)
             ORDER BY u.email",
        )
        .bind(input.user_id)
        .bind(input.provider_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(ListOutput { items })
    }
}
