//! The kernel's own module: shared entities and the generic commands every
//! installation has.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use erpcraft_core::{Error, Result};
use erpcraft_db::Migration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::audit;
use crate::authz::{Access, PermissionRule};
use crate::command::{Command, CommandSpec, Ctx, DynCommand, command};
use crate::document::{self, Document, DocumentChange, DocumentQuery, DocumentView, NewDocument};
use crate::entity::{self, ListQuery, Page};
use crate::outbox;
use crate::policy;
use crate::registry::{Description, EntityMeta, FieldMeta, FieldType, Module, Registry};

pub(crate) struct KernelModule;

impl Module for KernelModule {
    fn name(&self) -> &'static str {
        "kernel"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "kernel",
                sql: include_str!("../migrations/0001_kernel.sql"),
            },
            Migration {
                version: 2,
                name: "audit",
                sql: include_str!("../migrations/0002_audit.sql"),
            },
            Migration {
                version: 3,
                name: "outbox_delivery",
                sql: include_str!("../migrations/0003_outbox_delivery.sql"),
            },
            Migration {
                version: 4,
                name: "document_link",
                sql: include_str!("../migrations/0004_document_link.sql"),
            },
            Migration {
                version: 5,
                name: "policy_rule",
                sql: include_str!("../migrations/0005_policy_rule.sql"),
            },
            Migration {
                version: 6,
                name: "document_summary",
                sql: include_str!("../migrations/0006_document_summary.sql"),
            },
            Migration {
                version: 7,
                name: "approval",
                sql: include_str!("../migrations/0007_approval.sql"),
            },
            Migration {
                version: 8,
                name: "summary_version",
                sql: include_str!("../migrations/0008_summary_version.sql"),
            },
            Migration {
                version: 9,
                name: "approval_binding",
                sql: include_str!("../migrations/0009_approval_binding.sql"),
            },
        ]
    }

    fn policies(&self) -> Vec<policy::PolicyPoint> {
        policy::kernel_points()
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
                .order_by(&["code"])
                .search(&["code", "name"]),
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
                .search(&["code", "name"])
                .company_field("id"),
            crate::users::entity(),
            EntityMeta::new("role", "Role")
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new(
                    "permissions",
                    "Permission rules",
                    FieldType::Json,
                ))
                .field(created())
                .order_by(&["name"])
                .search(&["name"])
                // What a role gives is for those who may give it to read.
                .read_needs_global()
                .write_check(crate::users::check_role_write)
                .validator(validate_role),
            // How a company configures the policy points that modules
            // declare. Without a company the rule applies to every company.
            EntityMeta::new("policy_rule", "Policy rule")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company")).immutable(),
                )
                .field(FieldMeta::new("point", "Policy point", FieldType::Text).required())
                .field(
                    FieldMeta::new(
                        "effect",
                        "Effect",
                        FieldType::options(&["allow", "refuse", "approve"]),
                    )
                    .help("approve: the command waits until the approvers named here approve it.")
                    .required(),
                )
                .field(FieldMeta::new("condition", "When", FieldType::Text))
                .field(FieldMeta::new(
                    "message",
                    "Message when refused",
                    FieldType::Text,
                ))
                .field(
                    FieldMeta::new(
                        "approver_role_id",
                        "Approved by role",
                        FieldType::link("role"),
                    )
                    .help("For approve: holders of this role approve, for the company or all."),
                )
                .field(
                    FieldMeta::new(
                        "approver_permission",
                        "Approved by permission",
                        FieldType::Text,
                    )
                    .help("For approve: holders of this permission for the company approve."),
                )
                .field(
                    FieldMeta::new("approvals_needed", "Approvals needed", FieldType::Int)
                        .help("For approve: how many distinct people, never the requester."),
                )
                .field(
                    FieldMeta::new("approval_days", "Days to approve", FieldType::Int)
                        .help("For approve: the request expires after this many days (14)."),
                )
                .field(FieldMeta::new("priority", "Priority", FieldType::Int))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["point", "priority"])
                .search(&["point", "message"])
                .written_together(&["point", "condition"])
                .validator(policy::validate_rule),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        let mut commands = vec![
            command(Describe),
            command(EntityList),
            command(EntityGet),
            command(EntityCreate),
            command(EntityUpdate),
            command(EntityDelete),
            command(DocumentList),
            command(DocumentGet),
            command(DocumentCreate),
            command(DocumentUpdate),
            command(DocumentDelete),
            command(RefreshSummaries),
            command(Search),
            command(UserAssignRole),
            command(UserSetAttributes),
        ];
        commands.extend(audit::commands());
        commands.extend(outbox::commands());
        commands.extend(crate::users::commands());
        commands.extend(crate::approval::commands());
        commands
    }
}

// -------------------------------------------------------------- maintenance

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RefreshSummariesInput {
    /// Only documents of this type.
    doc_type: Option<String>,
    /// At most this many documents, 200 when omitted, 1000 at most.
    limit: Option<i64>,
}

struct RefreshSummaries;

#[async_trait]
impl Command for RefreshSummaries {
    type Input = RefreshSummariesInput;
    type Output = crate::document::SummariesRefreshed;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "document.refresh_summaries",
            "Work out the summary of documents that have none, written before their type kept \
             one, or whose summary is out of date because their type now keeps other figures. \
             One batch per call; call again while `remaining` is above zero. \
             The server also does this by itself after it starts.",
        )
        .permission(REFRESH_SUMMARIES)
        .label("Refresh summaries")
        .target(false)
    }

    async fn run(
        &self,
        ctx: &mut Ctx<'_>,
        input: RefreshSummariesInput,
    ) -> Result<crate::document::SummariesRefreshed> {
        ctx.actor.require_global(REFRESH_SUMMARIES)?;
        let done = crate::document::refresh_missing_summaries(
            ctx,
            input.doc_type.as_deref(),
            input.limit.unwrap_or(200),
        )
        .await?;
        if done.refreshed == 0 {
            ctx.no_change();
        }
        Ok(done)
    }
}

/// Documents are shared across companies only in their table, so this is
/// granted for all companies or not at all.
const REFRESH_SUMMARIES: &str = "document:refresh_summaries";

// ---------------------------------------------------------------- discovery

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DescribeInput {
    /// Limit the description to one module.
    module: Option<String>,
}

/// The system, and what the caller could do in it.
#[derive(Serialize, JsonSchema)]
struct Described {
    #[serde(flatten)]
    description: Description,
    access: Access,
}

struct Describe;

#[async_trait]
impl Command for Describe {
    type Input = DescribeInput;
    type Output = Described;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "meta.describe",
            "List the installed modules with their entities, document types and commands, and \
             under `access` what you could do with each: actions, fields and commands. \
             `policies` lists the business judgements a company can configure with \
             `policy_rule` records, with the facts each one supplies. Pass a module name to \
             see only that module.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DescribeInput) -> Result<Described> {
        let kernel = ctx.kernel;
        let mut description = kernel.registry().describe(input.module.as_deref());
        // What only the tables know: how many decimal places a column
        // keeps, and what it starts as when nothing is given.
        for entity in &mut description.entities {
            let table = kernel.tables().get(&mut *ctx.conn, &entity.table).await?;
            for field in &mut entity.fields {
                if let (Some(decimal), Some(scale)) = (&mut field.decimal, table.scale(&field.name))
                {
                    decimal.scale = Some(scale);
                }
                if field.default.is_none() && !field.readonly {
                    field.default = table.default(&field.name).cloned();
                }
            }
        }
        let access = Access::of(ctx.actor, &description);
        Ok(Described {
            description,
            access,
        })
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
    /// Text to look for in the fields the entity is searched by (see
    /// `search` in `meta.describe`), ignoring case.
    q: Option<String>,
    /// A condition each record must meet, as an expression over `record`:
    /// `record.account_type == 'asset' && !record.is_group`. Supports
    /// comparisons, `in`, `&&`, `||`, `!`, `user.<attribute>` and
    /// `params.<name>`; the text tests `contains`, `startsWith`, `endsWith`
    /// and, ignoring case, `containsIgnoreCase`, `startsWithIgnoreCase`,
    /// `endsWithIgnoreCase`, `equalsIgnoreCase` (`record.name.containsIgnoreCase('bolt')`);
    /// and dates relative to today: `today()`, `addDays(d, n)`,
    /// `addMonths(d, n)`, `startOfMonth(d)`, `endOfMonth(d)`, and the same
    /// for `Quarter` and `Year`.
    #[serde(rename = "where")]
    condition: Option<String>,
    /// Values the condition reads as `params.<name>`, bound as parameters
    /// so that a long list does not lengthen it: `{"customers": ["…"]}`
    /// for `record.customer_id in params.customers`.
    #[serde(default)]
    params: Map<String, Value>,
    /// The reader's time zone, such as `Europe/Berlin`: the day `today()`
    /// is in the condition, and where a day starts when a timestamp field
    /// is compared with one (`record.created_at >= '2026-11-18'`). The
    /// database's own zone when omitted.
    time_zone: Option<String>,
    /// Field names to sort by; prefix one with `-` for descending.
    #[serde(default)]
    sort: Vec<String>,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Records to skip, for paging.
    offset: Option<i64>,
    /// Leave out records that are no longer to be used: those whose
    /// `active_field` (see `meta.describe`) is false. What a lookup wants;
    /// a list that manages the records shows them all.
    #[serde(default)]
    active_only: bool,
}

struct EntityList;

#[async_trait]
impl Command for EntityList {
    type Input = EntityListInput;
    type Output = Page<Value>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "entity.list",
            "List records of an entity: filter by field values, search by text, sort and page. \
             Returns the page as `items` and the size of the whole list as `total`.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EntityListInput) -> Result<Page<Value>> {
        let query = ListQuery {
            filters: input.filters.into_iter().collect(),
            q: input.q,
            condition: input.condition,
            sort: input.sort,
            limit: input.limit,
            offset: input.offset,
            active_only: input.active_only,
            skip_total: false,
            params: Value::Object(input.params),
            time_zone: input.time_zone,
        };
        entity::list(ctx, &input.entity, &query).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityGetInput {
    /// Entity name, for example `account`.
    entity: String,
    /// Id of the record.
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
    /// Entity name, for example `account`.
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
    /// Entity name, for example `account`.
    entity: String,
    /// Id of the record.
    id: Uuid,
    /// Only the fields to change.
    data: Value,
    /// The `_version` of the record as last read. When given, the update is
    /// refused if the record has changed since.
    version: Option<String>,
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
        entity::update(
            ctx,
            &input.entity,
            input.id,
            input.data,
            input.version.as_deref(),
        )
        .await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntityDeleteInput {
    /// Entity name, for example `account`.
    entity: String,
    /// Id of the record.
    id: Uuid,
    /// The `_version` of the record as last read. When given, the delete is
    /// refused if the record has changed since.
    version: Option<String>,
}

struct EntityDelete;

#[async_trait]
impl Command for EntityDelete {
    type Input = EntityDeleteInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "entity.delete",
            "Delete one record of an entity. Refused while anything still refers to the record.",
        )
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: EntityDeleteInput) -> Result<Value> {
        entity::delete(ctx, &input.entity, input.id, input.version.as_deref()).await?;
        Ok(json!({ "deleted": input.id }))
    }
}

// ---------------------------------------------------------------- documents

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentListInput {
    /// Document type, for example `journal_entry`.
    doc_type: String,
    /// Only documents of this company.
    company_id: Option<Uuid>,
    /// Only documents in this state of the workflow, for example `draft`.
    status: Option<String>,
    /// Text to look for in the number and the memo, ignoring case.
    q: Option<String>,
    /// A condition over the document header, as an expression over
    /// `record`: `record.doc_date >= '2026-01-01' && record.status == 'draft'`.
    /// The header fields are id, company_id, number, status, locked,
    /// version, doc_date, memo, created_by, created_at and updated_at; the
    /// type's summary fields are `record.summary.<field>`
    /// (`record.summary.total_net > '1000'`). The language is the one of
    /// `entity.list`'s `where`, with its text tests and dates.
    #[serde(rename = "where")]
    condition: Option<String>,
    /// Values the condition reads as `params.<name>`, bound as parameters
    /// so that a long list does not lengthen it: `{"customers": ["…"]}`
    /// for `record.customer_id in params.customers`.
    #[serde(default)]
    params: Map<String, Value>,
    /// The reader's time zone, such as `Europe/Berlin`: the day `today()`
    /// is in the condition, and where a day starts when a timestamp field
    /// is compared with one (`record.created_at >= '2026-11-18'`). The
    /// database's own zone when omitted.
    time_zone: Option<String>,
    /// Sort by `number`, `status`, `doc_date`, `created_at`, `updated_at`
    /// or a field of the type's summary as `summary.<field>`; prefix one
    /// with `-` for descending. Newest first when omitted.
    #[serde(default)]
    sort: Vec<String>,
    /// At most 500; 50 when omitted.
    limit: Option<i64>,
    /// Records to skip, for paging.
    offset: Option<i64>,
    /// Summary field to the exact value it must have, for the fields the
    /// document type declares under `summary` in `meta.describe`:
    /// `{"vendor_id": "..."}`.
    #[serde(default)]
    summary: BTreeMap<String, String>,
}

struct DocumentList;

#[async_trait]
impl Command for DocumentList {
    type Input = DocumentListInput;
    type Output = Page<Document>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.list",
            "List documents of one type without their bodies: filter, search by text, sort and \
             page. Returns the page as `items` and the size of the whole list as `total`.",
        )
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentListInput) -> Result<Page<Document>> {
        let query = DocumentQuery {
            company_id: input.company_id,
            status: input.status,
            q: input.q,
            condition: input.condition,
            sort: input.sort,
            limit: input.limit,
            offset: input.offset,
            summary: input.summary.into_iter().collect(),
            skip_total: false,
            params: Value::Object(input.params),
            time_zone: input.time_zone,
        };
        document::list(ctx, &input.doc_type, &query).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentRef {
    /// Document type, for example `journal_entry`.
    doc_type: String,
    /// Id of the document.
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
    /// Document type, for example `journal_entry`.
    doc_type: String,
    /// The company the document belongs to.
    company_id: Uuid,
    /// When the business event happened.
    doc_date: NaiveDate,
    /// A note for people; searched by lists.
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
    /// Document type, for example `journal_entry`.
    doc_type: String,
    /// Id of the document.
    id: Uuid,
    /// The version last read. The update is refused if the document has
    /// changed since.
    version: i32,
    /// When the business event happened. Unchanged when omitted.
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
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocumentRef) -> Result<Value> {
        document::delete(ctx, &input.doc_type, input.id).await?;
        Ok(json!({ "deleted": input.id }))
    }
}

// ------------------------------------------------------------------- search

/// Fewest characters a search takes: one letter matches nearly everything.
const SEARCH_MIN_CHARS: usize = 2;
const SEARCH_PER_KIND: i64 = 5;
const SEARCH_PER_KIND_MAX: i64 = 10;
/// Most matches one search returns, over all kinds.
const SEARCH_CAP: usize = 50;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    /// Text to look for, ignoring case: in the fields each entity is
    /// searched by, and in the number and memo of documents. At least 2
    /// characters.
    q: String,
    /// Only what belongs to this company, and records shared by all
    /// companies. Everything you may read when omitted.
    company_id: Option<Uuid>,
    /// Matches to return per entity and per document type: 5 when
    /// omitted, at most 10.
    limit: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
struct SearchHit {
    id: Uuid,
    /// What to show for the match: a record's display fields, a
    /// document's number.
    title: String,
    /// More to tell it apart: a document's memo.
    subtitle: Option<String>,
    /// The company it belongs to; null for a record shared by all.
    company_id: Option<Uuid>,
    /// For a document: its state and date.
    status: Option<String>,
    doc_date: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema)]
struct SearchGroup {
    /// `entity` or `document`.
    kind: &'static str,
    /// The entity or the document type, to build a link from with the
    /// match's `id`.
    name: String,
    /// What the matches are, in the plural.
    label: String,
    items: Vec<SearchHit>,
    /// There are more matches of this kind than are returned.
    more: bool,
}

#[derive(Serialize, JsonSchema)]
struct SearchOutput {
    /// One group per entity and document type that has a match, entities
    /// first.
    groups: Vec<SearchGroup>,
    /// The search stopped at 50 matches; kinds after that were not
    /// searched. Ask for something more specific.
    truncated: bool,
}

struct Search;

/// A kind of record the caller cannot search, for want of the right to
/// read it or its search fields, is simply not in the result.
fn skip_unsearchable<T>(result: Result<entity::Page<T>>) -> Result<Option<entity::Page<T>>> {
    match result {
        Ok(page) => Ok(Some(page)),
        Err(Error::Forbidden(_) | Error::Validation(_)) => Ok(None),
        Err(other) => Err(other),
    }
}

#[async_trait]
impl Command for Search {
    type Input = SearchInput;
    type Output = SearchOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "search.records",
            "Find records and documents of every kind in one call: records by the fields their \
             entity is searched by (a code, a name), documents by number and memo. Returns the \
             best few matches per entity and per document type, each with the id and a title \
             to link to it. Only what you may read is searched, with the same row and field \
             rules as the lists. For more matches of one kind, use entity.list or \
             document.list with the same text as `q`.",
        )
        .label("Search")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SearchInput) -> Result<SearchOutput> {
        let q = input.q.trim().to_owned();
        if q.chars().count() < SEARCH_MIN_CHARS {
            return Err(Error::field(
                "q",
                format_args!("needs at least {SEARCH_MIN_CHARS} characters"),
            ));
        }
        let per_kind = input
            .limit
            .unwrap_or(SEARCH_PER_KIND)
            .clamp(1, SEARCH_PER_KIND_MAX);
        let (kernel, actor) = (ctx.kernel, ctx.actor);
        let mut groups: Vec<SearchGroup> = vec![];
        let mut found = 0;
        let mut truncated = false;
        // Take a page of one kind into the result; says whether to go on.
        let mut take = |mut group: SearchGroup| {
            group.more = group.items.len() as i64 > per_kind;
            group.items.truncate(per_kind as usize);
            let room = SEARCH_CAP - found;
            if group.items.len() > room {
                group.items.truncate(room);
                group.more = true;
            }
            found += group.items.len();
            if !group.items.is_empty() {
                groups.push(group);
            }
            found < SEARCH_CAP
        };

        // The lists do the searching, so their permissions are the
        // search's: rows out of reach are not found, and a field that is
        // withheld is neither searched nor shown.
        for meta in kernel.registry().entities() {
            if meta.search.is_empty() || !actor.can_any(&format!("{}:read", meta.name)) {
                continue;
            }
            let mut query = entity::ListQuery {
                q: Some(q.clone()),
                limit: Some(per_kind + 1),
                skip_total: true,
                ..Default::default()
            };
            let company_field = meta.company_field.as_deref().filter(|f| *f != "id");
            if let (Some(company), Some(field)) = (input.company_id, company_field) {
                query.filters.push((field.to_owned(), company.to_string()));
            }
            let Some(page) = skip_unsearchable(entity::list(ctx, &meta.name, &query).await)? else {
                continue;
            };
            let items = page
                .items
                .iter()
                .filter_map(|row| {
                    let id = row.get("id")?.as_str()?.parse().ok()?;
                    let shown: Vec<&str> = meta
                        .display
                        .iter()
                        .filter_map(|field| row.get(field)?.as_str())
                        .filter(|text| !text.is_empty())
                        .collect();
                    Some(SearchHit {
                        id,
                        title: shown.join(" · "),
                        subtitle: None,
                        company_id: company_field
                            .and_then(|field| row.get(field)?.as_str()?.parse().ok()),
                        status: None,
                        doc_date: None,
                    })
                })
                .collect();
            let more = take(SearchGroup {
                kind: "entity",
                name: meta.name.clone(),
                label: meta.plural.clone(),
                items,
                more: false,
            });
            if !more {
                truncated = true;
                break;
            }
        }

        if !truncated {
            for entry in kernel.registry().doc_types() {
                let meta = &entry.meta;
                if !actor.can_any(&format!("{}:read", meta.name)) {
                    continue;
                }
                let query = DocumentQuery {
                    company_id: input.company_id,
                    q: Some(q.clone()),
                    limit: Some(per_kind + 1),
                    skip_total: true,
                    ..Default::default()
                };
                let Some(page) = skip_unsearchable(document::list(ctx, &meta.name, &query).await)?
                else {
                    continue;
                };
                let items = page
                    .items
                    .into_iter()
                    .map(|doc| SearchHit {
                        id: doc.id,
                        title: doc.number.unwrap_or_else(|| meta.label.clone()),
                        subtitle: doc.memo,
                        company_id: Some(doc.company_id),
                        status: Some(doc.status),
                        doc_date: Some(doc.doc_date),
                    })
                    .collect();
                let more = take(SearchGroup {
                    kind: "document",
                    name: meta.name.clone(),
                    label: meta.plural.clone(),
                    items,
                    more: false,
                });
                if !more {
                    truncated = true;
                    break;
                }
            }
        }
        Ok(SearchOutput { groups, truncated })
    }
}

// ------------------------------------------------------------ authorization

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserAssignRoleInput {
    /// The user who receives the role.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "user", "key": "id" }))]
    user_id: Uuid,
    /// The role to grant.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "role", "key": "id" }))]
    role_id: Uuid,
    /// Omit to grant the role for every company.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "company", "key": "id" }))]
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
            "Grant a role to a user, for one company or for all companies. Granting a role \
             the user already holds there changes nothing. Someone who is not an \
             administrator grants only roles whose every right they hold themselves, at least \
             as far, and never to themselves.",
        )
        .permission("user:assign_role")
        .label("Assign role")
        .resource("user")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UserAssignRoleInput) -> Result<Value> {
        ctx.actor.require_global("user:assign_role")?;
        ctx.touch("user", input.user_id);
        crate::users::may_hand_on(ctx, input.user_id, input.role_id, input.company_id).await?;
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
    /// The user whose attributes are set.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "user", "key": "id" }))]
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
            "Set the facts about a user that permission rules can refer to. They widen what \
             rules such as `record.warehouse in user.warehouses` reach, so someone who is not \
             an administrator sets them only for another user who is not an administrator and \
             all of whose roles they could have granted.",
        )
        .permission("user:update")
        .label("Set attributes")
        .resource("user")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UserSetAttributesInput) -> Result<Value> {
        ctx.actor.require_global("user:update")?;
        ctx.touch("user", input.user_id);
        crate::users::may_administer(ctx, input.user_id).await?;
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
    let items = permissions.as_array().ok_or_else(|| {
        Error::validation("permissions must be a list of rules").with_field("permissions")
    })?;
    for (index, item) in items.iter().enumerate() {
        // The error names the list and which rule in it, counted from 1.
        check_permission_rule(registry, item).map_err(|error| {
            error
                .with_field("permissions")
                .with_line("permissions", index + 1)
        })?;
    }
    Ok(())
}

/// One permission rule of a role.
fn check_permission_rule(registry: &Registry, item: &Value) -> Result<()> {
    let rule = PermissionRule::from_json(item)?;
    if rule.condition.is_none() && rule.fields.is_none() {
        return Ok(());
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
    Ok(())
}
