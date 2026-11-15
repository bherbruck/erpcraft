//! Documents and postings.
//!
//! A **document** is a business record with a workflow its module defines:
//! a purchase order may be approved, partly received, closed. A **posting**
//! is the immutable record that a document changed a ledger. A document can
//! post many times over its life; a posting is undone only by a second
//! posting that reverses it.

use std::collections::BTreeMap;
use std::fmt::Write;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use erpcraft_core::{Error, Result, new_id};
use erpcraft_db::PgConnection;
use erpcraft_rules::{Bind, ColumnType};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Arguments;
use uuid::Uuid;

use crate::Kernel;
use crate::authz::RowScope;
use crate::command::Ctx;
use crate::entity::{DEFAULT_LIMIT, MAX_LIMIT, arguments};
use crate::numbering;
use crate::registry::DocTypeMeta;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Posting {
    pub id: Uuid,
    pub company_id: Uuid,
    pub document_id: Uuid,
    pub command: String,
    pub posting_date: NaiveDate,
    pub memo: Option<String>,
    pub reverses_id: Option<Uuid>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

/// A document with its type-specific body and everything it has posted.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentView {
    #[serde(flatten)]
    pub header: Document,
    pub body: Value,
    pub postings: Vec<Posting>,
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
}

/// A ledger that hangs entries off postings. The kernel calls every ledger
/// when a posting is reversed, so no module can forget one.
#[async_trait]
pub trait Ledger: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// Write entries under `reversal` that cancel those under `original`.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()>;
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

/// A document header as rules see it.
fn rule_record(doc: &Document) -> Value {
    serde_json::to_value(doc).unwrap_or(Value::Null)
}

fn clean_memo(memo: Option<String>) -> Option<String> {
    memo.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty())
}

pub async fn view(
    conn: &mut PgConnection,
    kernel: &Kernel,
    header: Document,
) -> Result<DocumentView> {
    let entry = kernel.registry().doc_type(&header.doc_type)?;
    let body = entry.handler.load_body(&mut *conn, &header).await?;
    let postings = postings_for(&mut *conn, header.id).await?;
    Ok(DocumentView {
        header,
        body,
        postings,
    })
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
    entry.handler.save_body(ctx, &doc, &new.body).await?;
    view(ctx.conn, kernel, doc).await
}

/// Load a document and lock its row until the transaction ends, after
/// checking `action` against the document's company. Every command that
/// changes a document starts here, which is what makes two concurrent
/// commands on one document run one after the other.
pub async fn lock(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid, action: &str) -> Result<Document> {
    ctx.kernel.registry().doc_type(doc_type)?;
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
        return Err(Error::conflict(format!(
            "the document is {} and can no longer be changed",
            doc.status
        )));
    }
    if doc.version != change.version {
        return Err(Error::conflict(format!(
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
    if let Some(body) = &change.body {
        entry.handler.save_body(ctx, &doc, body).await?;
    }
    view(ctx.conn, kernel, doc).await
}

pub async fn get(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid) -> Result<DocumentView> {
    let kernel = ctx.kernel;
    kernel.registry().doc_type(doc_type)?;
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
    view(ctx.conn, kernel, doc).await
}

pub async fn list(
    ctx: &mut Ctx<'_>,
    doc_type: &str,
    company_id: Option<Uuid>,
    status: Option<&str>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<Document>> {
    ctx.kernel.registry().doc_type(doc_type)?;
    let columns = rule_columns();
    let scope = RowScope {
        alias: "t",
        columns: &columns,
        company_column: Some("company_id"),
    };

    let mut binds = vec![Bind::Text(Some(doc_type.to_owned()))];
    let mut sql = String::from("SELECT t.* FROM document AS t WHERE t.doc_type = $1::text");
    if let Some(company) = company_id {
        binds.push(Bind::Text(Some(company.to_string())));
        let _ = write!(sql, " AND t.company_id = ${}::uuid", binds.len());
    }
    if let Some(status) = status {
        binds.push(Bind::Text(Some(status.to_owned())));
        let _ = write!(sql, " AND t.status = ${}::text", binds.len());
    }
    let permission = format!("{doc_type}:read");
    if let Some(filter) = ctx
        .actor
        .row_filter(&permission, &scope, false, &mut binds)?
    {
        let _ = write!(sql, " AND {filter}");
    }
    let _ = write!(
        sql,
        " ORDER BY t.created_at DESC, t.id DESC LIMIT ${} OFFSET ${}",
        binds.len() + 1,
        binds.len() + 2
    );
    let mut arguments = arguments(&binds)?;
    arguments
        .add(limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))
        .and_then(|()| arguments.add(offset.unwrap_or(0).max(0)))
        .map_err(Error::internal)?;
    let docs = sqlx::query_as_with::<_, Document, _>(&sql, arguments)
        .fetch_all(&mut *ctx.conn)
        .await?;
    Ok(docs)
}

/// Delete a document that is not locked and has never posted.
pub async fn delete(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid) -> Result<()> {
    let doc = lock(ctx, doc_type, id, "delete").await?;
    if doc.locked {
        return Err(Error::conflict(format!(
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
pub async fn transition(ctx: &mut Ctx<'_>, doc: &mut Document, to: &str) -> Result<()> {
    let meta = &ctx.kernel.registry().doc_type(&doc.doc_type)?.meta;
    let allowed = meta
        .transitions
        .iter()
        .any(|(from, target)| from == &doc.status && target == to);
    let state = meta
        .state(to)
        .filter(|_| allowed)
        .ok_or_else(|| Error::conflict(format!("a {} document cannot become {to}", doc.status)))?;
    *doc = sqlx::query_as(
        "UPDATE document SET status = $2, locked = $3, version = version + 1, updated_at = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(doc.id)
    .bind(&state.name)
    .bind(state.locked)
    .fetch_one(&mut *ctx.conn)
    .await?;
    Ok(())
}

/// Open a posting for a document. The caller then writes ledger entries
/// under it; each ledger verifies its own rules when the transaction commits.
pub async fn create_posting(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    posting_date: NaiveDate,
    memo: Option<String>,
) -> Result<Posting> {
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
        return Err(Error::conflict("a reversal cannot itself be reversed"));
    }
    let reversed: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM posting WHERE reverses_id = $1)")
            .bind(original.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
    if reversed {
        return Err(Error::conflict("the posting is already reversed"));
    }
    let reversal: Posting = sqlx::query_as(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, memo, reverses_id, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *",
    )
    .bind(new_id())
    .bind(original.company_id)
    .bind(original.document_id)
    .bind(ctx.command)
    .bind(posting_date.unwrap_or(original.posting_date))
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

pub async fn postings_for(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Posting>> {
    let postings =
        sqlx::query_as("SELECT * FROM posting WHERE document_id = $1 ORDER BY created_at, id")
            .bind(document_id)
            .fetch_all(&mut *conn)
            .await?;
    Ok(postings)
}
