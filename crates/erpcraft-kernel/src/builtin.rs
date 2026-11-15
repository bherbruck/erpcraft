//! The kernel's own module: shared entities and the generic commands every
//! installation has.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use erpcraft_core::{Error, Result};
use erpcraft_db::Migration;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::authz::PermissionRule;
use crate::command::{Command, CommandSpec, Ctx, DynCommand, command};
use crate::document::{self, DocumentChange, DocumentView, NewDocument};
use crate::entity;
use crate::registry::{Description, EntityMeta, FieldMeta, FieldType, Module, Registry};

pub(crate) struct KernelModule;

impl Module for KernelModule {
    fn name(&self) -> &'static str {
        "kernel"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "kernel",
            sql: include_str!("../migrations/0001_kernel.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("currency", "Currency")
                .field(
                    FieldMeta::new("code", "Code", FieldType::Text)
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new("decimals", "Decimal places", FieldType::Int).immutable())
                .field(created())
                .order_by(&["code"]),
            EntityMeta::new("company", "Company")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new(
                        "base_currency",
                        "Accounting currency",
                        FieldType::link_by("currency", "code"),
                    )
                    .required()
                    .immutable(),
                )
                .field(created())
                .order_by(&["code"])
                .company_field("id"),
            EntityMeta::new("role", "Role")
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new(
                    "permissions",
                    "Permission rules",
                    FieldType::Json,
                ))
                .field(created())
                .order_by(&["name"])
                .validator(validate_role),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Describe),
            command(EntityList),
            command(EntityGet),
            command(EntityCreate),
            command(EntityUpdate),
            command(DocumentList),
            command(DocumentGet),
            command(DocumentCreate),
            command(DocumentUpdate),
            command(DocumentDelete),
            command(UserAssignRole),
            command(UserSetAttributes),
        ]
    }
}

// ---------------------------------------------------------------- discovery

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DescribeInput {
    /// Limit the description to one module.
    module: Option<String>,
}

struct Describe;

#[async_trait]
impl Command for Describe {
    type Input = DescribeInput;
    type Output = Description;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "meta.describe",
            "List the installed modules with their entities, document types and commands. \
             Pass a module name to see only that module.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DescribeInput) -> Result<Description> {
        Ok(ctx.kernel.registry().describe(input.module.as_deref()))
    }
}

// ----------------------------------------------------------------- entities

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityListInput {
    /// Entity name, for example `account`.
    entity: String,
    /// Field name to the exact value it must have.
    #[serde(default)]
    filters: BTreeMap<String, String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

struct EntityList;

#[async_trait]
impl Command for EntityList {
    type Input = EntityListInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "entity.list",
            "List records of an entity, optionally filtered by field values.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EntityListInput) -> Result<Value> {
        let filters: Vec<(String, String)> = input.filters.into_iter().collect();
        let items = entity::list(ctx, &input.entity, &filters, input.limit, input.offset).await?;
        Ok(json!({ "items": items }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityGetInput {
    entity: String,
    id: Uuid,
}

struct EntityGet;

#[async_trait]
impl Command for EntityGet {
    type Input = EntityGetInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query("entity.get", "Read one record of an entity by id.")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EntityGetInput) -> Result<Value> {
        entity::get(ctx, &input.entity, input.id).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityCreateInput {
    entity: String,
    /// Field values. See `meta.describe` for the fields of each entity.
    data: Value,
}

struct EntityCreate;

#[async_trait]
impl Command for EntityCreate {
    type Input = EntityCreateInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action("entity.create", "Create a record of an entity.")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EntityCreateInput) -> Result<Value> {
        entity::create(ctx, &input.entity, input.data).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityUpdateInput {
    entity: String,
    id: Uuid,
    /// Only the fields to change.
    data: Value,
}

struct EntityUpdate;

#[async_trait]
impl Command for EntityUpdate {
    type Input = EntityUpdateInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action("entity.update", "Change fields of one record of an entity.")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EntityUpdateInput) -> Result<Value> {
        entity::update(ctx, &input.entity, input.id, input.data).await
    }
}

// ---------------------------------------------------------------- documents

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentListInput {
    doc_type: String,
    company_id: Option<Uuid>,
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

struct DocumentList;

#[async_trait]
impl Command for DocumentList {
    type Input = DocumentListInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.list",
            "List documents of one type, newest first, without their bodies.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentListInput) -> Result<Value> {
        let items = document::list(
            ctx,
            &input.doc_type,
            input.company_id,
            input.status.as_deref(),
            input.limit,
            input.offset,
        )
        .await?;
        Ok(json!({ "items": items }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentRef {
    doc_type: String,
    id: Uuid,
}

struct DocumentGet;

#[async_trait]
impl Command for DocumentGet {
    type Input = DocumentRef;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.get",
            "Read one document with its body and its postings.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentRef) -> Result<DocumentView> {
        document::get(ctx, &input.doc_type, input.id).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentCreateInput {
    doc_type: String,
    company_id: Uuid,
    /// When the business event happened.
    doc_date: NaiveDate,
    memo: Option<String>,
    /// Type-specific content. See `meta.describe` for each document type.
    #[serde(default)]
    body: Value,
}

struct DocumentCreate;

#[async_trait]
impl Command for DocumentCreate {
    type Input = DocumentCreateInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "document.create",
            "Create a document in the first state of its workflow.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentCreateInput) -> Result<DocumentView> {
        document::create(
            ctx,
            NewDocument {
                doc_type: input.doc_type,
                company_id: input.company_id,
                doc_date: input.doc_date,
                memo: input.memo,
                body: input.body,
            },
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentUpdateInput {
    doc_type: String,
    id: Uuid,
    /// The version last read. The update is refused if the document has
    /// changed since.
    version: i32,
    doc_date: Option<NaiveDate>,
    /// An empty string clears the memo.
    memo: Option<String>,
    /// Replaces the whole body when given.
    body: Option<Value>,
}

struct DocumentUpdate;

#[async_trait]
impl Command for DocumentUpdate {
    type Input = DocumentUpdateInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "document.update",
            "Change a document that is still editable.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentUpdateInput) -> Result<DocumentView> {
        document::update(
            ctx,
            DocumentChange {
                doc_type: input.doc_type,
                id: input.id,
                version: input.version,
                doc_date: input.doc_date,
                memo: input.memo,
                body: input.body,
            },
        )
        .await
    }
}

struct DocumentDelete;

#[async_trait]
impl Command for DocumentDelete {
    type Input = DocumentRef;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "document.delete",
            "Delete a document that is still editable and has never posted.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentRef) -> Result<Value> {
        document::delete(ctx, &input.doc_type, input.id).await?;
        Ok(json!({ "deleted": input.id }))
    }
}

// ------------------------------------------------------------ authorization

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserAssignRoleInput {
    user_id: Uuid,
    role_id: Uuid,
    /// Omit to grant the role for every company.
    company_id: Option<Uuid>,
}

struct UserAssignRole;

#[async_trait]
impl Command for UserAssignRole {
    type Input = UserAssignRoleInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "user.assign_role",
            "Grant a role to a user, for one company or for all companies.",
        )
        .permission("user:assign_role")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UserAssignRoleInput) -> Result<Value> {
        ctx.actor.require_global("user:assign_role")?;
        sqlx::query(
            "INSERT INTO role_assignment (id, user_id, role_id, company_id) VALUES ($1, $2, $3, $4)
             ON CONFLICT DO NOTHING",
        )
        .bind(erpcraft_core::new_id())
        .bind(input.user_id)
        .bind(input.role_id)
        .bind(input.company_id)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(json!({ "assigned": true }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserSetAttributesInput {
    user_id: Uuid,
    /// Replaces all of the user's attributes. Permission rules read them as
    /// `user.<name>`, for example `{"warehouses": ["W1", "W2"]}`.
    attributes: Map<String, Value>,
}

struct UserSetAttributes;

#[async_trait]
impl Command for UserSetAttributes {
    type Input = UserSetAttributesInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "user.set_attributes",
            "Set the facts about a user that permission rules can refer to.",
        )
        .permission("user:update")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UserSetAttributesInput) -> Result<Value> {
        ctx.actor.require_global("user:update")?;
        let attributes = Value::Object(input.attributes);
        let updated = sqlx::query("UPDATE app_user SET attributes = $2 WHERE id = $1")
            .bind(input.user_id)
            .bind(&attributes)
            .execute(&mut *ctx.conn)
            .await?
            .rows_affected();
        if updated == 0 {
            return Err(Error::not_found("user"));
        }
        Ok(json!({ "user_id": input.user_id, "attributes": attributes }))
    }
}

/// Check the permission rules of a role when it is saved, so that a stored
/// rule can always be read, evaluated and compiled into a query.
fn validate_role(registry: &Registry, data: &Map<String, Value>) -> Result<()> {
    let Some(permissions) = data.get("permissions") else {
        return Ok(());
    };
    let items = permissions
        .as_array()
        .ok_or_else(|| Error::validation("permissions must be a list of rules"))?;
    for item in items {
        let rule = PermissionRule::from_json(item)?;
        if rule.condition.is_none() && rule.fields.is_none() {
            continue;
        }
        let resource = rule.resource().ok_or_else(|| {
            Error::validation(format!(
                "permission rule {item}: a rule with 'where' or 'fields' must name one resource, not '*'"
            ))
        })?;
        let columns = registry.rule_columns(resource).ok_or_else(|| {
            Error::validation(format!(
                "permission rule {item}: '{resource}' is not an entity or a document type, \
                 so the rule cannot have 'where' or 'fields'"
            ))
        })?;
        if let Some(condition) = &rule.condition {
            condition.check(&columns)?;
        }
        if let Some(fields) = &rule.fields {
            let entity = registry.entity(resource).map_err(|_| {
                Error::validation(format!(
                    "permission rule {item}: field rules apply to entities; '{resource}' is a document type"
                ))
            })?;
            for field in fields {
                if !entity.fields.iter().any(|f| &f.name == field) {
                    return Err(Error::validation(format!(
                        "permission rule {item}: {resource} has no field '{field}'"
                    )));
                }
            }
        }
    }
    Ok(())
}
