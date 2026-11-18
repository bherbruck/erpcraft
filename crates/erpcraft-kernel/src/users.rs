//! Administering the user directory: who the users are, which roles they
//! hold, and switching them off.
//!
//! Users are also the read-only entity `user` (the table `app_user`), so a
//! field can link to one and generic lists can find them. The commands here
//! add what a generic read cannot: the roles each user holds, and the
//! changes, with the rules that keep at least one administrator able to
//! sign in. How a user signs in is not the kernel's business: an identity
//! module shows that itself (the auth module's `sso_identity.list`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use erpcraft_core::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::authz::{Actor, PermissionRule};
use crate::command::{Command, CommandSpec, Ctx, DynCommand, command};
use crate::entity::{Page, like_pattern};
use crate::registry::{EntityMeta, FieldMeta, FieldType};
use serde_json::Map;

const READ: &str = "user:read";
const UPDATE: &str = "user:update";
const ASSIGN: &str = "user:assign_role";
const MAX_LIMIT: i64 = 500;

pub(crate) fn commands() -> Vec<std::sync::Arc<dyn DynCommand>> {
    vec![
        command(List),
        command(Get),
        command(Update),
        command(SetActive { active: false }),
        command(SetActive { active: true }),
        command(RevokeRole),
    ]
}

/// The read-only entity over the directory. Credentials are not in it:
/// they belong to the identity module.
pub(crate) fn entity() -> EntityMeta {
    let mut meta = EntityMeta::new("user", "User")
        .field(FieldMeta::new("email", "Email", FieldType::Text))
        .field(FieldMeta::new("name", "Name", FieldType::Text))
        .field(FieldMeta::new("is_admin", "Administrator", FieldType::Bool))
        .field(FieldMeta::new("active", "Active", FieldType::Bool))
        .field(
            FieldMeta::new("attributes", "Attributes", FieldType::Json)
                .help("Facts about the user that permission rules read as user.<name>."),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["name", "email"])
        .search(&["name", "email"])
        // The directory is the installation's, not a company's: reading
        // it takes user:read for all companies, as user.list does.
        .read_needs_global()
        .read_only();
    meta.table = "app_user".into();
    meta
}

// ---------------------------------------------------------------- reading

/// One role a user holds.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RoleGrant {
    /// Role id.
    pub role_id: Uuid,
    /// Role name.
    pub role_name: String,
    /// The company the role is held for; null for all companies.
    pub company_id: Option<Uuid>,
    /// That company's code; null for all companies.
    pub company_code: Option<String>,
}

/// A user as an administrator sees one. No credentials.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct UserView {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    /// An administrator may do everything; permission rules do not apply.
    pub is_admin: bool,
    /// An inactive user cannot sign in, and their tokens stop working.
    pub active: bool,
    pub created_at: DateTime<Utc>,
    /// The roles the user holds.
    #[sqlx(json)]
    pub roles: Vec<RoleGrant>,
    /// Facts permission rules read as `user.<name>`. Only from `user.get`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[sqlx(default)]
    pub attributes: Option<Value>,
}

const VIEW: &str = "SELECT u.id, u.email, u.name, u.is_admin, u.active, u.created_at,
        COALESCE((SELECT jsonb_agg(jsonb_build_object(
                    'role_id', r.id, 'role_name', r.name,
                    'company_id', ra.company_id, 'company_code', c.code)
                  ORDER BY r.name, c.code NULLS FIRST)
                  FROM role_assignment ra JOIN role r ON r.id = ra.role_id
                  LEFT JOIN company c ON c.id = ra.company_id
                  WHERE ra.user_id = u.id), '[]') AS roles";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// Text to look for in the name and the email address.
    q: Option<String>,
    /// Only active users (true) or only inactive ones (false). Both when
    /// omitted.
    active: Option<bool>,
    /// Only these users, whatever else is asked: to show users already
    /// chosen.
    ids: Option<Vec<Uuid>>,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Users to skip, for paging.
    offset: Option<i64>,
}

struct List;

#[async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = Page<UserView>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "user.list",
            "The users, by name, with the roles each holds and for which company. Search by \
             name or email, filter by active. Never returns credentials. Takes user:read for \
             all companies.",
        )
        .permission(READ)
        .label("Users")
        .resource("user")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ListInput) -> Result<Page<UserView>> {
        ctx.actor.require_global(READ)?;
        if input
            .ids
            .as_ref()
            .is_some_and(|ids| ids.len() > MAX_LIMIT as usize)
        {
            return Err(Error::field("ids", "can name at most 500 users"));
        }
        let q = input
            .q
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(like_pattern);
        const WHERE: &str = "($1::text IS NULL OR u.name ILIKE $1 OR u.email ILIKE $1)
             AND ($2::bool IS NULL OR u.active = $2)
             AND ($3::uuid[] IS NULL OR u.id = ANY($3))";
        let total: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM app_user u WHERE {WHERE}"))
                .bind(&q)
                .bind(input.active)
                .bind(&input.ids)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let items = sqlx::query_as(&format!(
            "{VIEW} FROM app_user u WHERE {WHERE} ORDER BY u.name, u.email, u.id
             LIMIT $4 OFFSET $5"
        ))
        .bind(&q)
        .bind(input.active)
        .bind(&input.ids)
        .bind(input.limit.unwrap_or(50).clamp(1, MAX_LIMIT))
        .bind(input.offset.unwrap_or(0).max(0))
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(Page { items, total })
    }
}

async fn view(ctx: &mut Ctx<'_>, id: Uuid) -> Result<UserView> {
    sqlx::query_as(&format!(
        "{VIEW}, u.attributes FROM app_user u WHERE u.id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *ctx.conn)
    .await?
    .ok_or_else(|| Error::not_found("user"))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserRef {
    /// User id.
    id: Uuid,
}

struct Get;

#[async_trait]
impl Command for Get {
    type Input = UserRef;
    type Output = UserView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "user.get",
            "One user with the roles they hold and their attributes. How they sign in is \
             shown by the identity module (sso_identity.list). Takes user:read for all \
             companies.",
        )
        .permission(READ)
        .label("User")
        .resource("user")
        .target(true)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UserRef) -> Result<UserView> {
        ctx.actor.require_global(READ)?;
        view(ctx, input.id).await
    }
}

// ------------------------------------------------------------ escalation

/// The rules of a role as stored.
fn role_rules(permissions: &Value) -> Result<Vec<PermissionRule>> {
    permissions
        .as_array()
        .map(|rules| rules.iter().map(PermissionRule::from_json).collect())
        .unwrap_or_else(|| Ok(vec![]))
}

/// Whether the actor may grant or revoke `role_id` for `company_id` to
/// `user_id`: an administrator may; anyone else only for someone else,
/// and only a role they hold all of themselves. See
/// [`Actor::may_delegate`].
pub(crate) async fn may_hand_on(
    ctx: &mut Ctx<'_>,
    user_id: Uuid,
    role_id: Uuid,
    company_id: Option<Uuid>,
) -> Result<()> {
    if ctx.actor.is_admin {
        return Ok(());
    }
    if user_id == ctx.actor.user_id {
        return Err(Error::forbidden(
            "you cannot grant or revoke a role for yourself",
        ));
    }
    let permissions: Option<Value> =
        sqlx::query_scalar("SELECT permissions FROM role WHERE id = $1")
            .bind(role_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    let Some(permissions) = permissions else {
        return Err(Error::field("role_id", "must be a role"));
    };
    ctx.actor
        .may_delegate(&role_rules(&permissions)?, company_id)
        .map_err(Error::forbidden)
}

/// Whether the actor may change facts about `user_id` that rules read:
/// an administrator may; anyone else not about themselves, not about an
/// administrator, and only about a user all of whose roles they could
/// have granted. Attributes widen what rules such as
/// `record.warehouse in user.warehouses` let a user reach, so setting them
/// is handing on rights.
pub(crate) async fn may_administer(ctx: &mut Ctx<'_>, user_id: Uuid) -> Result<()> {
    if ctx.actor.is_admin {
        return Ok(());
    }
    if user_id == ctx.actor.user_id {
        return Err(Error::forbidden(
            "you cannot change your own attributes: they decide what your rules reach",
        ));
    }
    let target: Option<bool> = sqlx::query_scalar("SELECT is_admin FROM app_user WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
    match target {
        None => return Err(Error::not_found("user")),
        Some(true) => {
            return Err(Error::forbidden(
                "only an administrator can change an administrator",
            ));
        }
        Some(false) => {}
    }
    let held: Vec<(Option<Uuid>, Value)> = sqlx::query_as(
        "SELECT ra.company_id, r.permissions FROM role_assignment ra
         JOIN role r ON r.id = ra.role_id WHERE ra.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    for (company_id, permissions) in held {
        ctx.actor
            .may_delegate(&role_rules(&permissions)?, company_id)
            .map_err(|why| {
                Error::forbidden(format!(
                    "the user holds a role you could not have granted ({why})"
                ))
            })?;
    }
    Ok(())
}

/// [`EntityMeta::write_check`] of `role`: a role is written, changed or
/// deleted only by someone who holds all it gives, for all companies; an
/// administrator is not limited.
pub(crate) fn check_role_write(
    actor: &Actor,
    before: Option<&Value>,
    data: &Map<String, Value>,
) -> Result<()> {
    if actor.is_admin {
        return Ok(());
    }
    if let Some(permissions) = before.and_then(|b| b.get("permissions")) {
        actor
            .may_delegate(&role_rules(permissions)?, None)
            .map_err(|why| Error::forbidden(format!("you cannot change this role: {why}")))?;
    }
    if let Some(permissions) = data.get("permissions") {
        actor
            .may_delegate(&role_rules(permissions)?, None)
            .map_err(|why| {
                Error::forbidden(format!("the role would give more than you hold: {why}"))
                    .with_field("permissions")
            })?;
    }
    Ok(())
}

// --------------------------------------------------------------- changing

/// Lock the user, and refuse a change that would leave no active
/// administrator: nobody could then grant administrator rights again.
async fn lock(ctx: &mut Ctx<'_>, id: Uuid) -> Result<(bool, bool)> {
    let row: Option<(bool, bool)> =
        sqlx::query_as("SELECT is_admin, active FROM app_user WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    row.ok_or_else(|| Error::not_found("user"))
}

async fn keep_an_administrator(ctx: &mut Ctx<'_>, id: Uuid) -> Result<()> {
    // Every active administrator, locked, so that two administrators
    // removing each other at once cannot both succeed.
    let others: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM app_user WHERE is_admin AND active AND id <> $1 ORDER BY id FOR UPDATE",
    )
    .bind(id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    if others.is_empty() {
        return Err(Error::state(
            "this is the last active administrator: make another user an administrator first",
        ));
    }
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateInput {
    /// User id.
    id: Uuid,
    /// The user's name, as shown to others.
    name: Option<String>,
    /// Make the user an administrator, or no longer one. Only an
    /// administrator may change this, and the last active administrator
    /// cannot stop being one.
    is_admin: Option<bool>,
}

struct Update;

#[async_trait]
impl Command for Update {
    type Input = UpdateInput;
    type Output = UserView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "user.update",
            "Change a user's name, or whether they are an administrator. Only an \
             administrator may grant or take away administrator rights, and the last active \
             administrator keeps them. Takes user:update for all companies. To switch a user \
             off, use user.deactivate.",
        )
        .permission(UPDATE)
        .label("Edit user")
        .resource("user")
        .target(true)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UpdateInput) -> Result<UserView> {
        ctx.actor.require_global(UPDATE)?;
        ctx.touch("user", input.id);
        let (was_admin, active) = lock(ctx, input.id).await?;
        if let Some(name) = &input.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 200 {
                return Err(Error::field("name", "must have 1 to 200 characters"));
            }
            sqlx::query("UPDATE app_user SET name = $2 WHERE id = $1")
                .bind(input.id)
                .bind(name)
                .execute(&mut *ctx.conn)
                .await?;
        }
        if let Some(is_admin) = input.is_admin
            && is_admin != was_admin
        {
            if !ctx.actor.is_admin {
                return Err(Error::forbidden(
                    "only an administrator can grant or take away administrator rights",
                ));
            }
            if was_admin && active {
                keep_an_administrator(ctx, input.id).await?;
            }
            sqlx::query("UPDATE app_user SET is_admin = $2 WHERE id = $1")
                .bind(input.id)
                .bind(is_admin)
                .execute(&mut *ctx.conn)
                .await?;
        }
        view(ctx, input.id).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetActiveInput {
    /// User id.
    id: Uuid,
}

struct SetActive {
    active: bool,
}

#[async_trait]
impl Command for SetActive {
    type Input = SetActiveInput;
    type Output = UserView;

    fn spec(&self) -> CommandSpec {
        if self.active {
            CommandSpec::action(
                "user.activate",
                "Let a deactivated user sign in again. Their tokens that have not expired work \
                 again. Activating an administrator takes an administrator. Takes user:update \
                 for all companies.",
            )
            .permission(UPDATE)
            .label("Activate")
            .resource("user")
            .target(true)
        } else {
            CommandSpec::action(
                "user.deactivate",
                "Switch a user off: they cannot sign in, and every token they hold stops \
                 working at once. Nothing they did is undone, and user.activate switches them \
                 on again. The last active administrator cannot be deactivated. Deactivating \
                 an administrator takes an administrator. Takes user:update for all companies.",
            )
            .permission(UPDATE)
            .label("Deactivate")
            .resource("user")
            .target(true)
            .destructive()
        }
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetActiveInput) -> Result<UserView> {
        ctx.actor.require_global(UPDATE)?;
        ctx.touch("user", input.id);
        let (is_admin, active) = lock(ctx, input.id).await?;
        if is_admin && !ctx.actor.is_admin {
            return Err(Error::forbidden(
                "only an administrator can switch an administrator on or off",
            ));
        }
        if active != self.active {
            if !self.active && is_admin {
                keep_an_administrator(ctx, input.id).await?;
            }
            sqlx::query("UPDATE app_user SET active = $2 WHERE id = $1")
                .bind(input.id)
                .bind(self.active)
                .execute(&mut *ctx.conn)
                .await?;
        }
        view(ctx, input.id).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RevokeRoleInput {
    /// The user who holds the role.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "user", "key": "id" }))]
    user_id: Uuid,
    /// The role to take away.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "role", "key": "id" }))]
    role_id: Uuid,
    /// The company it was held for; omit for the grant for all companies.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "company", "key": "id" }))]
    company_id: Option<Uuid>,
}

struct RevokeRole;

#[async_trait]
impl Command for RevokeRole {
    type Input = RevokeRoleInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "user.revoke_role",
            "Take a role away from a user, for one company or the grant for all companies. \
             The user's requests from now on no longer have it. Revoking a role the user does \
             not hold changes nothing. Takes user:assign_role for all companies.",
        )
        .permission(ASSIGN)
        .label("Revoke role")
        .resource("user")
        .target(false)
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RevokeRoleInput) -> Result<Value> {
        ctx.actor.require_global(ASSIGN)?;
        may_hand_on(ctx, input.user_id, input.role_id, input.company_id).await?;
        ctx.touch("user", input.user_id);
        let revoked = sqlx::query(
            "DELETE FROM role_assignment
             WHERE user_id = $1 AND role_id = $2 AND company_id IS NOT DISTINCT FROM $3",
        )
        .bind(input.user_id)
        .bind(input.role_id)
        .bind(input.company_id)
        .execute(&mut *ctx.conn)
        .await?
        .rows_affected();
        if revoked == 0 {
            ctx.no_change();
        }
        Ok(json!({ "user_id": input.user_id, "revoked": revoked > 0 }))
    }
}
