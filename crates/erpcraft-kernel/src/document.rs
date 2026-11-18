//! Documents and postings.
//!
//! A **document** is a business record with a workflow its module defines:
//! a purchase order may be approved, partly received, closed. A **posting**
//! is the immutable record that a document changed a ledger. A document can
//! post many times over its life; a posting is undone only by a second
//! posting that reverses it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use erpcraft_core::{Error, Result, new_id};
use erpcraft_db::PgConnection;
use erpcraft_rules::{Bind, ColumnType};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Arguments;
use uuid::Uuid;

use crate::authz::{RowScope, Target};
use crate::command::Ctx;
use crate::entity::{
    DEFAULT_LIMIT, MAX_LIMIT, Page, SortKey, arguments, caller_condition, like_pattern, sort_key,
};
use crate::numbering;
use crate::placeholders::checked;
use crate::registry::{DocTypeMeta, FieldMeta, FieldType};

#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct Document {
    pub id: Uuid,
    pub company_id: Uuid,
    pub doc_type: String,
    pub number: Option<String>,
    pub status: String,
    pub locked: bool,
    pub version: i32,
    pub doc_date: NaiveDate,
    pub memo: Option<String>,
    pub created_by: Uuid,
    /// The name of the user who created the document, for whoever may
    /// read the document. Present where a document is read or listed.
    #[sqlx(default)]
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The figures the document's type keeps with every document: see
    /// `summary` of the type in `meta.describe`. A figure that is not
    /// known yet, such as a value that posting sets, is null.
    pub summary: Value,
}

#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct Posting {
    pub id: Uuid,
    pub company_id: Uuid,
    pub document_id: Uuid,
    pub command: String,
    pub posting_date: NaiveDate,
    pub memo: Option<String>,
    pub reverses_id: Option<Uuid>,
    pub created_by: Uuid,
    /// The name of the user who posted, where a posting is read with its
    /// document.
    #[sqlx(default)]
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A document with its type-specific body, everything it has posted, and
/// the documents it came from or led to.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DocumentView {
    #[serde(flatten)]
    pub header: Document,
    /// The content the document's type defines. See the fields and
    /// collections of the type in `meta.describe`.
    pub body: Value,
    pub postings: Vec<PostingView>,
    /// Only the linked documents the reader may read.
    pub links: Vec<LinkedDocument>,
}

/// A posting with what it wrote to the ledgers.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PostingView {
    #[serde(flatten)]
    pub posting: Posting,
    /// The entries each ledger holds under this posting, by ledger name
    /// (`gl`, `stock`, `open_items`, `tax`). A ledger with no entries
    /// under the posting is left out, and so is one whose entries the
    /// reader may not read.
    pub entries: BTreeMap<String, Vec<Value>>,
}

/// A document linked to the one being viewed.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LinkedDocument {
    /// `source`: the viewed document follows from this one. `result`: this
    /// one follows from the viewed document.
    pub relation: &'static str,
    /// What the later document is to the earlier one: `receipt`, `invoice`.
    pub kind: String,
    pub id: Uuid,
    pub doc_type: String,
    pub number: Option<String>,
    pub status: String,
    pub doc_date: NaiveDate,
}

/// What a module supplies for a document type: its description and how its
/// body is stored.
#[async_trait]
pub trait DocumentHandler: Send + Sync + 'static {
    fn meta(&self) -> DocTypeMeta;

    /// Validate and store the body of an unlocked document, replacing any
    /// previous body.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()>;

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value>;

    /// The figures to keep with the document's header, as an object with
    /// the fields the type declares in [`DocTypeMeta::summary`]: who the
    /// document is with, what it comes to. The kernel asks for them after
    /// the body is saved and after every change of state, and stores what
    /// it gets, so lists can show and sort by them without reading bodies.
    ///
    /// Work amounts out as the posting does, with the same rounding, so
    /// that the total shown is the total posted; leave a figure null while
    /// it is not known.
    async fn summary(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let _ = (conn, doc);
        Ok(json!({}))
    }

    /// The same figures, worked out from `body`: the document's body as
    /// [`DocumentHandler::load_body`] returned it a moment ago. The kernel
    /// calls this one; implement it to read the lines from the body instead
    /// of loading them again. By default it asks
    /// [`DocumentHandler::summary`].
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let _ = body;
        self.summary(conn, doc).await
    }
}

/// Reading a body as [`DocumentHandler::load_body`] returned it, for
/// [`DocumentHandler::summary_of`]: decimals are written as text.
pub mod body {
    use erpcraft_core::Decimal;
    use serde_json::Value;
    use uuid::Uuid;

    /// A decimal, given as text or as a number. `None` for null.
    pub fn decimal(value: &Value) -> Option<Decimal> {
        match value {
            Value::String(text) => text.parse().ok(),
            Value::Number(number) => number.to_string().parse().ok(),
            _ => None,
        }
    }

    /// A decimal that must be there; zero otherwise.
    pub fn amount(value: &Value) -> Decimal {
        decimal(value).unwrap_or_default()
    }

    pub fn uuid(value: &Value) -> Option<Uuid> {
        value.as_str().and_then(|text| text.parse().ok())
    }

    /// The lines of a collection, or none.
    pub fn lines<'b>(body: &'b Value, collection: &str) -> &'b [Value] {
        body[collection].as_array().map_or(&[], Vec::as_slice)
    }
}

/// A ledger that hangs entries off postings. The kernel calls every ledger
/// when a posting is reversed, so no module can forget one.
#[async_trait]
pub trait Ledger: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// What the ledger is called on a screen: "General ledger".
    fn label(&self) -> String {
        crate::registry::humanize(self.name())
    }

    /// The JSON Schema of one row that [`Ledger::entries`] returns, with a
    /// title on every property; see [`row_schema`]. It is published with
    /// the ledger in `meta.describe` and in the output schema of every
    /// command that returns a document. Open (any object) by default.
    fn entry_schema(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    /// Write entries under `reversal` that cancel those under `original`.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()>;

    /// The entries this ledger holds under `posting`, in order, as they
    /// are shown with the document: what a person needs to read to see
    /// what the posting did, with amounts as decimal strings.
    ///
    /// The reader of a document is not thereby a reader of the ledger.
    /// Return `None` if the actor may not read this ledger's entries for
    /// the posting's company, and leave out what is withheld from them;
    /// [`withhold`] does the latter for the fields of the ledger's entity.
    async fn entries(&self, ctx: &mut Ctx<'_>, posting: &Posting) -> Result<Option<Vec<Value>>> {
        let _ = (ctx, posting);
        Ok(Some(vec![]))
    }

    /// The tables this ledger writes its entries to, each with a
    /// `posting_id` column. A document is shown with a ledger's entries
    /// only for postings that have rows in one of them, so the ledger is
    /// not asked for postings it never wrote to; the tables say what was
    /// written, so nothing has to remember to mark it. A ledger that names
    /// none is asked for every posting.
    fn entry_tables(&self) -> &'static [&'static str] {
        &[]
    }
}

/// The schema of a row type for [`Ledger::entries`]: as it is written,
/// with a title on every property. A field the reader may not see is left
/// out of a row, so declare every field optional.
pub fn row_schema<T: schemars::JsonSchema>() -> Value {
    let schema = schemars::generate::SchemaSettings::default()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut schema = serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(map) = schema.as_object_mut() {
        map.remove("$schema");
    }
    crate::command::titled_throughout(schema)
}

/// For a ledger answering [`Ledger::entries`]: remove from every row the
/// fields that may be withheld from the actor under `permission`, such as
/// `stock_entry:read`. A field withheld on some records is removed from
/// all of them, as lists refuse to filter by such a field.
pub fn withhold(actor: &crate::authz::Actor, permission: &str, rows: &mut [Value]) {
    if !actor.limits_fields(permission) {
        return;
    }
    for row in rows {
        if let Value::Object(fields) = row {
            fields.retain(|name, _| !actor.may_withhold(permission, name));
        }
    }
}

/// The documents of one type (`$1`) whose summary is missing or was worked
/// out by another version (`$2`) of the type's summary. Written so that the
/// indexes of kernel migration 0011 answer it: `<>` would not use them.
pub const STALE_SUMMARIES: &str = "FROM document d WHERE d.doc_type = $1::text \
     AND (d.summary_version < $2::int4 OR d.summary_version > $2::int4 OR d.summary = '{}'::jsonb)";

/// What [`refresh_missing_summaries`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SummariesRefreshed {
    /// Documents whose summary was worked out in this batch.
    pub refreshed: i64,
    /// Documents that still have none.
    pub remaining: i64,
}

/// The most documents one batch refreshes, so that a batch holds its locks
/// only briefly.
pub const SUMMARY_BATCH_MAX: i64 = 1000;

/// Work out the summary of up to `limit` documents that have none (those
/// written before their type kept one) or one worked out by an older
/// version of their type's summary (see `DocTypeMeta::summary_version`).
/// `doc_type` limits it to one type.
/// Documents another transaction holds are skipped, not waited for.
pub async fn refresh_missing_summaries(
    ctx: &mut Ctx<'_>,
    doc_type: Option<&str>,
    limit: i64,
) -> Result<SummariesRefreshed> {
    let registry = ctx.kernel.registry();
    let types: Vec<String> = match doc_type {
        Some(name) => {
            registry.doc_type(name)?;
            vec![name.to_owned()]
        }
        None => registry.doc_types().map(|t| t.meta.name.clone()).collect(),
    };
    // Only types that keep a summary can lack one, and each with the
    // version of its summary now.
    let (types, versions): (Vec<String>, Vec<i32>) = types
        .into_iter()
        .filter_map(|name| {
            let entry = registry.doc_type(&name).ok()?;
            (!entry.meta.summary.is_empty()).then_some((name, entry.meta.summary_version))
        })
        .unzip();
    // Missing, or worked out by another version of the type's summary:
    // asked type by type, in a form the indexes `document_summary_version`
    // and `document_summary_missing` answer, so that when nothing is stale
    // (every start of the server) no document is read.
    let limit = limit.clamp(1, SUMMARY_BATCH_MAX);
    let mut refreshed = 0;
    let mut remaining = 0;
    for (name, version) in types.iter().zip(&versions) {
        if refreshed < limit {
            let docs: Vec<Document> = sqlx::query_as(&format!(
                "SELECT d.* {STALE_SUMMARIES} LIMIT $3 FOR UPDATE OF d SKIP LOCKED"
            ))
            .bind(name)
            .bind(version)
            .bind(limit - refreshed)
            .fetch_all(&mut *ctx.conn)
            .await?;
            for mut doc in docs {
                refresh_summary(ctx, &mut doc).await?;
                refreshed += 1;
            }
        }
        let left: i64 = sqlx::query_scalar(&format!("SELECT count(*) {STALE_SUMMARIES}"))
            .bind(name)
            .bind(version)
            .fetch_one(&mut *ctx.conn)
            .await?;
        remaining += left;
    }
    Ok(SummariesRefreshed {
        refreshed,
        remaining,
    })
}

/// One batch of [`refresh_missing_summaries`] as the server's own work, in
/// a transaction of its own: for the background worker.
pub async fn refresh_missing_summaries_batch(
    kernel: &crate::Kernel,
    limit: i64,
) -> Result<SummariesRefreshed> {
    let actor = crate::Actor {
        user_id: Uuid::nil(),
        email: String::new(),
        name: "the server".into(),
        is_admin: true,
        attributes: json!({}),
        grants: vec![],
        scopes: None,
        credential: None,
    };
    let mut tx = kernel.pool().begin().await?;
    let mut ctx = Ctx::internal(&mut tx, &actor, kernel, "document.refresh_summaries");
    let done = refresh_missing_summaries(&mut ctx, None, limit).await?;
    tx.commit().await?;
    Ok(done)
}

/// Ask the document's type for its summary and keep it with the header.
/// The kernel does this whenever a body is saved or a state changes; a
/// module calls it when something else changes what the summary shows.
pub async fn refresh_summary(ctx: &mut Ctx<'_>, doc: &mut Document) -> Result<()> {
    let entry = ctx.kernel.registry().doc_type(&doc.doc_type)?;
    if entry.meta.summary.is_empty() {
        return Ok(());
    }
    let body = entry.handler.load_body(&mut *ctx.conn, doc).await?;
    let mut summary = entry.handler.summary_of(&mut *ctx.conn, doc, &body).await?;
    // The view at the end of the command shows this body again unless the
    // document changes: every change of state or body comes back here.
    ctx.bodies.retain(|(id, _), _| *id != doc.id);
    ctx.bodies.insert((doc.id, doc.version), body);
    // An amount is written with the decimal places its field declares,
    // however it was worked out, so that the same amount always reads the
    // same: 9.4600, before posting and after.
    for field in &entry.meta.summary {
        let Some(scale) = field.decimal.as_ref().and_then(|d| d.scale) else {
            continue;
        };
        let amount = summary
            .get(&field.name)
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<erpcraft_core::Decimal>().ok());
        if let Some(mut amount) = amount {
            amount.rescale(scale);
            summary[&field.name] = json!(amount);
        }
    }
    if !summary.is_object() {
        return Err(Error::internal(format!(
            "the summary of a {} is not an object",
            doc.doc_type
        )));
    }
    sqlx::query("UPDATE document SET summary = $2, summary_version = $3 WHERE id = $1")
        .bind(doc.id)
        .bind(&summary)
        .bind(entry.meta.summary_version)
        .execute(&mut *ctx.conn)
        .await?;
    doc.summary = summary;
    Ok(())
}

/// The body as its handler is given it to save: what the caller sent,
/// without the fields the system sets. A document type returns those when
/// it is read (`qty_received` on an order line, `value` on a receipt line),
/// so a body that is sent back as it was read must be accepted; but they
/// are never taken from the caller. The `id` of a line stays: it says
/// which stored line the sent one is. See [`line_ids`].
fn writable_body(meta: &DocTypeMeta, body: &Value) -> Value {
    let Value::Object(body) = body else {
        return body.clone();
    };
    let mut body = body.clone();
    for field in meta.fields.iter().filter(|f| f.readonly) {
        body.remove(&field.name);
    }
    for collection in &meta.collections {
        // A collection the system fills in whole, such as the components
        // of a production order, is not the caller's to send.
        if collection.fields.iter().all(|f| f.readonly) {
            body.remove(&collection.name);
            continue;
        }
        let Some(Value::Array(rows)) = body.get_mut(&collection.name) else {
            continue;
        };
        for row in rows {
            if let Value::Object(row) = row {
                for field in &collection.fields {
                    if field.readonly && field.name != "id" {
                        row.remove(&field.name);
                    }
                }
            }
        }
    }
    Value::Object(body)
}

/// The ids to store the lines of a body under, in the order sent. A line
/// that was sent with the id of a line the document has in `table` keeps
/// that id, so that what refers to the line (another document's lines, a
/// ledger entry's `source_line_id`) stays valid when a draft is edited.
/// Any other line gets a new id: one sent without an id, with an id the
/// document does not have, or with an id an earlier line already took.
///
/// Call this before deleting the stored lines. `table` must have the
/// columns `id` and `document_id`; its name comes from the module, never
/// from the caller.
pub async fn line_ids(
    conn: &mut PgConnection,
    table: &str,
    document_id: Uuid,
    sent: impl IntoIterator<Item = Option<Uuid>>,
) -> Result<Vec<Uuid>> {
    let mut stored: Vec<Uuid> =
        sqlx::query_scalar(&format!("SELECT id FROM {table} WHERE document_id = $1"))
            .bind(document_id)
            .fetch_all(&mut *conn)
            .await?;
    Ok(sent
        .into_iter()
        .map(|id| {
            let kept = id.and_then(|id| stored.iter().position(|s| *s == id));
            match kept {
                Some(at) => stored.swap_remove(at),
                None => new_id(),
            }
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct NewDocument {
    pub doc_type: String,
    pub company_id: Uuid,
    pub doc_date: NaiveDate,
    pub memo: Option<String>,
    pub body: Value,
}

#[derive(Debug, Clone)]
pub struct DocumentChange {
    pub doc_type: String,
    pub id: Uuid,
    /// The version the caller last read.
    pub version: i32,
    pub doc_date: Option<NaiveDate>,
    /// `Some("")` clears the memo.
    pub memo: Option<String>,
    pub body: Option<Value>,
}

/// The header fields of a document that permission rules can refer to.
pub(crate) fn rule_columns() -> BTreeMap<String, ColumnType> {
    [
        ("id", ColumnType::Uuid),
        ("company_id", ColumnType::Uuid),
        ("doc_type", ColumnType::Text),
        ("number", ColumnType::Text),
        ("status", ColumnType::Text),
        ("locked", ColumnType::Bool),
        ("version", ColumnType::Int),
        ("doc_date", ColumnType::Date),
        ("memo", ColumnType::Text),
        ("created_by", ColumnType::Uuid),
        ("created_at", ColumnType::Timestamp),
        ("updated_at", ColumnType::Timestamp),
    ]
    .into_iter()
    .map(|(name, ty)| (name.to_owned(), ty))
    .collect()
}

/// The fields every document has, whatever its type: its header. They are
/// described like the fields of a body, so that an interface takes their
/// labels from here as it does every other label.
pub fn header_fields() -> Vec<FieldMeta> {
    let field = |name: &str, label: &str, ty: FieldType| FieldMeta::new(name, label, ty);
    vec![
        field("id", "ID", FieldType::Uuid).readonly(),
        field("company_id", "Company", FieldType::link("company"))
            .required()
            .immutable(),
        field("doc_type", "Type", FieldType::Text).readonly(),
        field("number", "Number", FieldType::Text)
            .readonly()
            .help("Given when the document is posted or approved."),
        field("status", "Status", FieldType::Text).readonly(),
        field("locked", "Locked", FieldType::Bool).readonly(),
        field("version", "Version", FieldType::Int).readonly(),
        field("doc_date", "Date", FieldType::Date)
            .required()
            .help("When the business event happened."),
        field("memo", "Memo", FieldType::Text),
        field("created_by", "Created by", FieldType::link("user")).readonly(),
        field("created_by_name", "Created by", FieldType::Text).readonly(),
        field("created_at", "Created", FieldType::Timestamp).readonly(),
        field("updated_at", "Changed", FieldType::Timestamp).readonly(),
    ]
}

/// A document header as rules see it.
fn rule_record(doc: &Document) -> Value {
    serde_json::to_value(doc).unwrap_or(Value::Null)
}

fn clean_memo(memo: Option<String>) -> Option<String> {
    memo.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty())
}

/// Blank what the documents' summaries copied from linked records the
/// reader may not read: a vendor's name on an order, for a reader who may
/// read the order but not that vendor (or not its name). See
/// [`FieldMeta::shown_from`](crate::FieldMeta::shown_from).
pub async fn redact_summaries(ctx: &mut Ctx<'_>, docs: &mut [&mut Document]) -> Result<()> {
    if ctx.actor.is_admin || docs.is_empty() {
        return Ok(());
    }
    let kernel = ctx.kernel;
    let Ok(entry) = kernel.registry().doc_type(&docs[0].doc_type) else {
        return Ok(());
    };
    for field in &entry.meta.summary {
        let Some(from) = &field.shown_from else {
            continue;
        };
        let Some(entity) = entry
            .meta
            .summary
            .iter()
            .find(|f| f.name == from.link)
            .and_then(|f| match &f.ty {
                FieldType::Link { entity, .. } => Some(entity.clone()),
                _ => None,
            })
        else {
            continue;
        };
        let withheld = ctx
            .actor
            .may_withhold(&format!("{entity}:read"), &from.field);
        let ids: Vec<Uuid> = docs
            .iter()
            .filter(|d| d.doc_type == entry.meta.name)
            .filter_map(|d| d.summary.get(&from.link).and_then(Value::as_str))
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect();
        let readable = if withheld {
            BTreeSet::new()
        } else {
            crate::entity::readable_ids(ctx, &entity, &ids).await?
        };
        for doc in docs.iter_mut().filter(|d| d.doc_type == entry.meta.name) {
            let visible = doc
                .summary
                .get(&from.link)
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .is_some_and(|id| readable.contains(&id));
            if !visible
                && let Some(summary) = doc.summary.as_object_mut()
                && summary.contains_key(&field.name)
            {
                summary.insert(field.name.clone(), Value::Null);
            }
        }
    }
    Ok(())
}

/// Whether the reader may filter or sort a list by `field` of the
/// summary: not by a copy of a linked record's field unless they read
/// every such record and that field, or the list would tell them what
/// they may not read.
fn may_query_summary(ctx: &Ctx<'_>, meta: &DocTypeMeta, field: &FieldMeta) -> Result<()> {
    let Some(from) = &field.shown_from else {
        return Ok(());
    };
    let entity = meta
        .summary
        .iter()
        .find(|f| f.name == from.link)
        .and_then(|f| match &f.ty {
            FieldType::Link { entity, .. } => ctx.kernel.registry().entity(entity).ok(),
            _ => None,
        });
    match entity {
        Some(entity) if crate::entity::reads_all(ctx.actor, entity, &from.field) => Ok(()),
        _ => Err(Error::forbidden(format!(
            "you may not filter or sort by '{}': it comes from records you may not read all of",
            field.name
        ))),
    }
}

pub async fn view(ctx: &mut Ctx<'_>, mut header: Document) -> Result<DocumentView> {
    let entry = ctx.kernel.registry().doc_type(&header.doc_type)?;
    redact_summaries(ctx, &mut [&mut header]).await?;
    // Whoever may read a document may know who made it, as with the
    // record's history.
    if header.created_by_name.is_none() && header.created_by == ctx.actor.user_id {
        // The caller's own name is known already.
        header.created_by_name = Some(ctx.actor.name.clone());
    } else if header.created_by_name.is_none() {
        header.created_by_name = sqlx::query_scalar("SELECT name FROM app_user WHERE id = $1")
            .bind(header.created_by)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    }
    if ctx.view == crate::command::View::Header {
        return Ok(DocumentView {
            header,
            body: Value::Null,
            postings: vec![],
            links: vec![],
        });
    }
    let body = match ctx.bodies.remove(&(header.id, header.version)) {
        Some(body) => body,
        None => entry.handler.load_body(&mut *ctx.conn, &header).await?,
    };
    let kernel = ctx.kernel;
    let ledgers = kernel.registry().ledgers();
    let postings_found = postings_for(&mut *ctx.conn, header.id).await?;
    let written = written_ledgers(ctx, ledgers, &postings_found).await?;
    let mut postings = vec![];
    for posting in postings_found {
        let mut entries = BTreeMap::new();
        for (index, ledger) in ledgers.iter().enumerate() {
            if !ledger.entry_tables().is_empty() && !written.contains(&(posting.id, index as i32)) {
                continue;
            }
            match ledger.entries(ctx, &posting).await? {
                Some(rows) if !rows.is_empty() => {
                    entries.insert(ledger.name().to_owned(), rows);
                }
                _ => {}
            }
        }
        postings.push(PostingView { posting, entries });
    }
    let links = links_of(ctx, header.id).await?;
    Ok(DocumentView {
        header,
        body,
        postings,
        links,
    })
}

/// Which ledgers, by their place in `ledgers`, hold entries under which of
/// `postings`, asked of the tables the ledgers name, in one statement.
async fn written_ledgers(
    ctx: &mut Ctx<'_>,
    ledgers: &[std::sync::Arc<dyn Ledger>],
    postings: &[Posting],
) -> Result<BTreeSet<(Uuid, i32)>> {
    let probes: Vec<String> = ledgers
        .iter()
        .enumerate()
        .flat_map(|(index, ledger)| {
            ledger.entry_tables().iter().map(move |table| {
                format!(
                    "SELECT p.id, {index} FROM unnest($1::uuid[]) AS p (id)
                     WHERE EXISTS (SELECT 1 FROM {table} e WHERE e.posting_id = p.id)"
                )
            })
        })
        .collect();
    if probes.is_empty() || postings.is_empty() {
        return Ok(BTreeSet::new());
    }
    let ids: Vec<Uuid> = postings.iter().map(|p| p.id).collect();
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(&probes.join(" UNION "))
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Set decimal columns of a document type's line table, for many lines in
/// one statement: `columns` names each column with a value per line of
/// `ids`, in the same order. For the values a posting works out per line.
/// Names are the caller's own, never input.
pub async fn set_line_values(
    conn: &mut PgConnection,
    table: &str,
    ids: &[Uuid],
    columns: &[(&str, &[Option<erpcraft_core::Decimal>])],
) -> Result<()> {
    if ids.is_empty() || columns.is_empty() {
        return Ok(());
    }
    let set: Vec<String> = columns
        .iter()
        .map(|(name, _)| format!("{name} = r.{name}"))
        .collect();
    let arrays: Vec<String> = (0..columns.len())
        .map(|n| format!("${}::numeric[]", n + 2))
        .collect();
    let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
    let sql = format!(
        "UPDATE {table} t SET {} FROM unnest($1::uuid[], {}) AS r (id, {}) WHERE t.id = r.id",
        set.join(", "),
        arrays.join(", "),
        names.join(", ")
    );
    crate::placeholders::check(&sql, 1 + columns.len())?;
    let mut query = sqlx::query(&sql).bind(ids);
    for (_, values) in columns {
        query = query.bind(*values);
    }
    query.execute(&mut *conn).await?;
    Ok(())
}

/// Record that document `to_id` follows from document `from_id`. Both must
/// belong to the same company. Linking twice changes nothing.
pub async fn link(ctx: &mut Ctx<'_>, from_id: Uuid, to_id: Uuid, kind: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO document_link (from_id, to_id, kind) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(kind)
    .execute(&mut *ctx.conn)
    .await?;
    Ok(())
}

/// The documents linked to `id`, sources first, that the actor may read.
pub async fn links_of(ctx: &mut Ctx<'_>, id: Uuid) -> Result<Vec<LinkedDocument>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        is_source: bool,
        kind: String,
        #[sqlx(flatten)]
        doc: Document,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT true AS is_source, l.kind, d.* FROM document_link l
         JOIN document d ON d.id = l.from_id WHERE l.to_id = $1
         UNION ALL
         SELECT false, l.kind, d.* FROM document_link l
         JOIN document d ON d.id = l.to_id WHERE l.from_id = $1
         ORDER BY is_source DESC, created_at, id",
    )
    .bind(id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            ctx.actor.can_on(
                &format!("{}:read", row.doc.doc_type),
                Target::Company(row.doc.company_id),
                &rule_record(&row.doc),
            )
        })
        .map(|row| LinkedDocument {
            relation: if row.is_source { "source" } else { "result" },
            kind: row.kind,
            id: row.doc.id,
            doc_type: row.doc.doc_type,
            number: row.doc.number,
            status: row.doc.status,
            doc_date: row.doc.doc_date,
        })
        .collect())
}

/// Which of these documents the actor may read: `{doc_type}:read` for the
/// document's company, checked against its header as for a linked
/// document, because a rule on a document can depend on it. A report or a
/// message that names the document behind a row (its type, number, memo)
/// names it only when its id is in the answer.
pub async fn readable(ctx: &mut Ctx<'_>, ids: &[Uuid]) -> Result<BTreeSet<Uuid>> {
    let docs: Vec<Document> = sqlx::query_as("SELECT * FROM document WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
    Ok(docs
        .into_iter()
        .filter(|doc| {
            ctx.actor.can_on(
                &format!("{}:read", doc.doc_type),
                Target::Company(doc.company_id),
                &rule_record(doc),
            )
        })
        .map(|doc| doc.id)
        .collect())
}

pub async fn create(ctx: &mut Ctx<'_>, new: NewDocument) -> Result<DocumentView> {
    let kernel = ctx.kernel;
    let entry = kernel.registry().doc_type(&new.doc_type)?;
    let state = entry
        .meta
        .state(&entry.meta.initial_state)
        .ok_or_else(|| Error::internal("initial state is not declared"))?;
    let memo = clean_memo(new.memo);
    // Rules are checked against the document as it is about to be created.
    let proposed = json!({
        "company_id": new.company_id,
        "doc_type": new.doc_type,
        "number": null,
        "status": state.name,
        "locked": state.locked,
        "version": 1,
        "doc_date": new.doc_date,
        "memo": memo,
        "created_by": ctx.actor.user_id,
    });
    ctx.require_on(
        &format!("{}:create", new.doc_type),
        new.company_id,
        &proposed,
    )?;

    let doc: Document = sqlx::query_as(
        "INSERT INTO document (id, company_id, doc_type, status, locked, doc_date, memo, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(new_id())
    .bind(new.company_id)
    .bind(&new.doc_type)
    .bind(&state.name)
    .bind(state.locked)
    .bind(new.doc_date)
    .bind(memo)
    .bind(ctx.actor.user_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    ctx.touch(&doc.doc_type, doc.id);
    entry
        .handler
        .save_body(ctx, &doc, &writable_body(&entry.meta, &new.body))
        .await?;
    let mut doc = doc;
    refresh_summary(ctx, &mut doc).await?;
    view(ctx, doc).await
}

/// Load a document and lock its row until the transaction ends, after
/// checking `action` against the document's company. Every command that
/// changes a document starts here, which is what makes two concurrent
/// commands on one document run one after the other.
pub async fn lock(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid, action: &str) -> Result<Document> {
    ctx.kernel.registry().doc_type(doc_type)?;
    ctx.touch(doc_type, id);
    let doc: Document =
        sqlx::query_as("SELECT * FROM document WHERE id = $1 AND doc_type = $2 FOR UPDATE")
            .bind(id)
            .bind(doc_type)
            .fetch_optional(&mut *ctx.conn)
            .await?
            .ok_or_else(|| Error::not_found(doc_type))?;
    ctx.require_on(
        &format!("{doc_type}:{action}"),
        doc.company_id,
        &rule_record(&doc),
    )?;
    Ok(doc)
}

pub async fn update(ctx: &mut Ctx<'_>, change: DocumentChange) -> Result<DocumentView> {
    let kernel = ctx.kernel;
    let entry = kernel.registry().doc_type(&change.doc_type)?;
    let doc = lock(ctx, &change.doc_type, change.id, "update").await?;
    if doc.locked {
        return Err(Error::state(format!(
            "the document is {} and can no longer be changed",
            doc.status
        )));
    }
    if doc.version != change.version {
        return Err(Error::stale(format!(
            "the document changed after version {} was read; it is now at version {}",
            change.version, doc.version
        )));
    }
    let doc: Document = sqlx::query_as(
        "UPDATE document SET
            doc_date = COALESCE($2, doc_date),
            memo = CASE WHEN $3 THEN $4 ELSE memo END,
            version = version + 1,
            updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(doc.id)
    .bind(change.doc_date)
    .bind(change.memo.is_some())
    .bind(clean_memo(change.memo))
    .fetch_one(&mut *ctx.conn)
    .await?;
    let mut doc = doc;
    if let Some(body) = &change.body {
        entry
            .handler
            .save_body(ctx, &doc, &writable_body(&entry.meta, body))
            .await?;
    }
    refresh_summary(ctx, &mut doc).await?;
    view(ctx, doc).await
}

/// Read the header of a document the actor may read.
pub async fn header(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid) -> Result<Document> {
    ctx.kernel.registry().doc_type(doc_type)?;
    let doc: Document = sqlx::query_as("SELECT * FROM document WHERE id = $1 AND doc_type = $2")
        .bind(id)
        .bind(doc_type)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found(doc_type))?;
    ctx.require_on(
        &format!("{doc_type}:read"),
        doc.company_id,
        &rule_record(&doc),
    )?;
    Ok(doc)
}

pub async fn get(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid) -> Result<DocumentView> {
    let doc = header(ctx, doc_type, id).await?;
    view(ctx, doc).await
}

/// What a list of documents is asked for.
#[derive(Debug, Clone, Default)]
pub struct DocumentQuery {
    pub company_id: Option<Uuid>,
    pub status: Option<String>,
    /// Text to look for in the number and the memo.
    pub q: Option<String>,
    /// A condition over the header as `record`, and `user`, in the language
    /// of permission rules: `record.doc_date >= '2026-01-01'`.
    pub condition: Option<String>,
    /// Header fields; a leading `-` sorts descending. Empty: newest first.
    pub sort: Vec<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Summary field and the exact value it must have, for the fields the
    /// type declares: the vendor of an order.
    pub summary: Vec<(String, String)>,
    /// Do not count the whole list: `total` is then the size of the page.
    pub skip_total: bool,
    /// Values the condition reads as `params.<name>`; null when none.
    pub params: Value,
    /// The reader's time zone (IANA), for `today()` and days against timestamps.
    pub time_zone: Option<String>,
}

/// The header fields a list of documents can be sorted by. A list can
/// also be sorted by a field of the type's summary, as `summary.<field>`.
pub const SORTABLE: &[&str] = &["number", "status", "doc_date", "created_at", "updated_at"];

pub async fn list(
    ctx: &mut Ctx<'_>,
    doc_type: &str,
    query: &DocumentQuery,
) -> Result<Page<Document>> {
    let meta = &ctx.kernel.registry().doc_type(doc_type)?.meta;
    // A summary field is named by the caller, so it must be one the type
    // declares before it goes into the query.
    let summary_field = |name: &str| {
        meta.summary.iter().find(|f| f.name == name).ok_or_else(|| {
            Error::validation(format!(
                "a {doc_type} has no summary field '{name}'; it has: {}",
                meta.summary
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
    };
    let columns = rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: Some("company_id"),
    };

    let mut binds = vec![Bind::Text(Some(doc_type.to_owned()))];
    let mut conditions = String::from("t.doc_type = $1::text");
    if let Some(company) = query.company_id {
        binds.push(Bind::Text(Some(company.to_string())));
        let _ = write!(conditions, " AND t.company_id = ${}::uuid", binds.len());
    }
    if let Some(status) = &query.status {
        binds.push(Bind::Text(Some(status.clone())));
        let _ = write!(conditions, " AND t.status = ${}::text", binds.len());
    }
    for (name, value) in &query.summary {
        let field = summary_field(name)?;
        may_query_summary(ctx, meta, field)?;
        binds.push(Bind::Text(Some(value.clone())));
        let _ = write!(
            conditions,
            " AND t.summary->>'{}' = ${}::text",
            field.name,
            binds.len()
        );
    }
    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        binds.push(Bind::Text(Some(like_pattern(q))));
        let n = binds.len();
        let _ = write!(
            conditions,
            " AND (t.number ILIKE ${n} OR t.memo ILIKE ${n})"
        );
    }
    if let Some(source) = &query.condition {
        // The summary's fields too, as `record.summary.<field>`, those the
        // reader may filter by: a copy of a linked record's field only for
        // one who reads every such record.
        let mut columns = columns.clone();
        for field in &meta.summary {
            if let Some(ty) = field.ty.column_type()
                && may_query_summary(ctx, meta, field).is_ok()
            {
                columns.insert(format!("summary.{}", field.name), ty);
            }
        }
        crate::entity::check_time_zone(&mut *ctx.conn, query.time_zone.as_deref()).await?;
        let inputs = crate::entity::ConditionInputs {
            params: &query.params,
            time_zone: query.time_zone.as_deref(),
        };
        let sql = caller_condition(ctx.actor, source, &columns, inputs, &mut binds)?;
        let _ = write!(conditions, " AND ({sql})");
    }
    let permission = format!("{doc_type}:read");
    if let Some(filter) = ctx
        .actor
        .row_filter(&permission, &scope, false, &mut binds)?
    {
        let _ = write!(conditions, " AND {filter}");
    }

    // Values the order needs are bound after the conditions', and only the
    // page's query uses them.
    let mut sort_binds = vec![];
    let mut order = vec![];
    for key in &query.sort {
        let SortKey {
            field: name,
            direction,
            nulls,
        } = sort_key(key)?;
        // Rows without a value go last unless the caller says otherwise.
        let nulls = if nulls.is_empty() {
            " NULLS LAST"
        } else {
            nulls
        };
        if let Some(field) = name.strip_prefix("summary.") {
            let mut field = summary_field(field)?;
            may_query_summary(ctx, meta, field)?;
            // A link sorts by the name the summary shows for it, when it
            // has one the reader may sort by.
            if matches!(field.ty, FieldType::Link { .. })
                && let Some(shown) = meta.summary.iter().find(|f| {
                    f.shown_from
                        .as_ref()
                        .is_some_and(|from| from.link == field.name)
                })
                && may_query_summary(ctx, meta, shown).is_ok()
            {
                field = shown;
            }
            // Amounts sort as numbers, everything else as text.
            let cast = match field.ty {
                FieldType::Decimal | FieldType::Int => "::numeric",
                _ => "",
            };
            order.push(format!(
                "(t.summary->>'{}'){cast} {direction}{nulls}",
                field.name
            ));
            continue;
        }
        if !SORTABLE.contains(&name) {
            return Err(Error::validation(format!(
                "cannot sort by '{name}'; use one of: {}, or summary.<field>",
                SORTABLE.join(", ")
            )));
        }
        if name == "status" {
            // States in the order of the workflow, not alphabetically.
            sort_binds.push(Bind::Texts(
                meta.states.iter().map(|s| s.name.clone()).collect(),
            ));
            order.push(format!(
                "array_position(${}::text[], t.status) {direction}{nulls}",
                binds.len() + sort_binds.len()
            ));
            continue;
        }
        order.push(format!("t.{name} {direction}{nulls}"));
    }
    if order.is_empty() {
        order.push("t.created_at DESC".into());
    }
    // The id makes the order total, so pages never overlap.
    order.push("t.id DESC".into());

    let total: Option<i64> = if query.skip_total {
        None
    } else {
        let sql = format!("SELECT count(*) FROM document AS t WHERE {conditions}");
        let count = sqlx::query_scalar_with(&sql, checked(&sql, arguments(&binds)?)?)
            // A list's SQL varies with its conditions and is planned with the
            // values each time: a cached generic plan would cast a bound list
            // per row and could not hash it.
            .persistent(false)
            .fetch_one(&mut *ctx.conn)
            .await?;
        Some(count)
    };

    binds.extend(sort_binds);
    let sql = format!(
        "SELECT t.*, u.name AS created_by_name FROM document AS t
         LEFT JOIN app_user u ON u.id = t.created_by
         WHERE {conditions} ORDER BY {} LIMIT ${} OFFSET ${}",
        order.join(", "),
        binds.len() + 1,
        binds.len() + 2
    );
    let mut arguments = arguments(&binds)?;
    arguments
        .add(query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
        .and_then(|()| arguments.add(query.offset.unwrap_or(0).max(0)))
        .map_err(Error::internal)?;
    let mut items = sqlx::query_as_with::<_, Document, _>(&sql, checked(&sql, arguments)?)
        // A list's SQL varies with its conditions and is planned with the
        // values each time: a cached generic plan would cast a bound list
        // per row and could not hash it.
        .persistent(false)
        .fetch_all(&mut *ctx.conn)
        .await?;
    redact_summaries(ctx, &mut items.iter_mut().collect::<Vec<_>>()).await?;
    Ok(Page {
        total: total.unwrap_or(items.len() as i64),
        items,
    })
}

/// Delete a document that is not locked and has never posted.
pub async fn delete(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid) -> Result<()> {
    let doc = lock(ctx, doc_type, id, "delete").await?;
    if doc.locked {
        return Err(Error::state(format!(
            "the document is {} and cannot be deleted",
            doc.status
        )));
    }
    sqlx::query("DELETE FROM document WHERE id = $1")
        .bind(doc.id)
        .execute(&mut *ctx.conn)
        .await?;
    Ok(())
}

/// Give a locked-for-update document its number, if it has none yet.
///
/// A command rarely needs this: [`transition`] numbers a document by
/// itself when it moves to a locked state, which is the last thing a
/// posting, approval or release command does. Call it only when the number
/// is needed before that, and know what it costs: the counter of the
/// series stays locked from this call until the transaction ends, so every
/// other command numbering a document of the same type, company and year
/// waits for the whole rest of this one. Take every other lock the command
/// needs first, so that the counter is always the last lock taken.
pub async fn assign_number(ctx: &mut Ctx<'_>, doc: &mut Document) -> Result<()> {
    if doc.number.is_some() {
        return Ok(());
    }
    let prefix = &ctx
        .kernel
        .registry()
        .doc_type(&doc.doc_type)?
        .meta
        .number_prefix;
    let number =
        numbering::next(&mut *ctx.conn, doc.company_id, prefix, doc.doc_date.year()).await?;
    sqlx::query("UPDATE document SET number = $2 WHERE id = $1")
        .bind(doc.id)
        .bind(&number)
        .execute(&mut *ctx.conn)
        .await?;
    doc.number = Some(number);
    Ok(())
}

/// Move a document to another state of its workflow.
///
/// A document that has no number yet receives one when it moves to a
/// locked state, unless the state is declared with
/// [`StateMeta::unnumbered`](crate::StateMeta::unnumbered), as "cancelled"
/// is. A command ends with its transition, so the counter of the
/// number series is locked only from here until the commit, not while the
/// command does its work.
pub async fn transition(ctx: &mut Ctx<'_>, doc: &mut Document, to: &str) -> Result<()> {
    let meta = &ctx.kernel.registry().doc_type(&doc.doc_type)?.meta;
    let allowed = meta
        .transitions
        .iter()
        .any(|(from, target)| from == &doc.status && target == to);
    let state = meta
        .state(to)
        .filter(|_| allowed)
        .ok_or_else(|| Error::state(format!("a {} document cannot become {to}", doc.status)))?;
    if state.assigns_number {
        assign_number(ctx, doc).await?;
    }
    *doc = sqlx::query_as(
        "UPDATE document SET status = $2, locked = $3, version = version + 1, updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(doc.id)
    .bind(&state.name)
    .bind(state.locked)
    .fetch_one(&mut *ctx.conn)
    .await?;
    refresh_summary(ctx, doc).await
}

/// Open a posting for a document. The caller then writes ledger entries
/// under it; each ledger verifies its own rules when the transaction commits.
pub async fn create_posting(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    posting_date: NaiveDate,
    memo: Option<String>,
) -> Result<Posting> {
    crate::policy::check_posting_date(ctx, doc.company_id, &doc.doc_type, posting_date, false)
        .await?;
    let posting = sqlx::query_as(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, memo, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(new_id())
    .bind(doc.company_id)
    .bind(doc.id)
    .bind(ctx.command)
    .bind(posting_date)
    .bind(clean_memo(memo))
    .bind(ctx.actor.user_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    Ok(posting)
}

/// Reverse a posting in every ledger. `posting_date` defaults to the date
/// of the original.
pub async fn reverse_posting(
    ctx: &mut Ctx<'_>,
    posting_id: Uuid,
    posting_date: Option<NaiveDate>,
    memo: Option<String>,
) -> Result<Posting> {
    let kernel = ctx.kernel;
    let original: Posting = sqlx::query_as("SELECT * FROM posting WHERE id = $1 FOR UPDATE")
        .bind(posting_id)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| Error::not_found("posting"))?;
    if original.reverses_id.is_some() {
        return Err(Error::state("a reversal cannot itself be reversed"));
    }
    let reversed: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM posting WHERE reverses_id = $1)")
            .bind(original.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
    if reversed {
        return Err(Error::state("the posting is already reversed"));
    }
    let posting_date = posting_date.unwrap_or(original.posting_date);
    let doc_type: String = sqlx::query_scalar("SELECT doc_type FROM document WHERE id = $1")
        .bind(original.document_id)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .unwrap_or_default();
    crate::policy::check_posting_date(ctx, original.company_id, &doc_type, posting_date, true)
        .await?;
    let reversal: Posting = sqlx::query_as(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, memo, reverses_id, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(new_id())
    .bind(original.company_id)
    .bind(original.document_id)
    .bind(ctx.command)
    .bind(posting_date)
    .bind(clean_memo(memo))
    .bind(original.id)
    .bind(ctx.actor.user_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    for ledger in kernel.registry().ledgers() {
        ledger.reverse(&mut *ctx.conn, &original, &reversal).await?;
    }
    Ok(reversal)
}

/// The posting a posted document made: the one that is not a reversal.
/// This is the posting to reverse when the document is reversed. For a
/// document that posts more than once it is the earliest such posting.
pub async fn original_posting(conn: &mut PgConnection, doc: &Document) -> Result<Posting> {
    postings_for(conn, doc.id)
        .await?
        .into_iter()
        .find(|p| p.reverses_id.is_none())
        .ok_or_else(|| Error::internal(format!("this {} has no posting", doc.doc_type)))
}

/// Everything a document has posted, reversals included, oldest first.
pub async fn postings_for(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Posting>> {
    let postings = sqlx::query_as(
        "SELECT p.*, u.name AS created_by_name FROM posting p
         LEFT JOIN app_user u ON u.id = p.created_by
         WHERE p.document_id = $1 ORDER BY p.created_at, p.id",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(postings)
}
