//! Accounting module. Uses only the public module API of the kernel and
//! the general ledger, exactly as a module outside this repository would.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, PgConnection, Result, StateMeta, async_trait,
    command, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const JOURNAL_ENTRY: &str = "journal_entry";

pub struct AccountingModule;

impl Module for AccountingModule {
    fn name(&self) -> &'static str {
        "accounting"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "accounting",
            sql: include_str!("../migrations/0001_accounting.sql"),
        }]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(JournalEntry)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(PostJournalEntry), command(ReverseJournalEntry)]
    }
}

// ------------------------------------------------------------ document type

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    account_id: Uuid,
    #[serde(default)]
    debit: Decimal,
    #[serde(default)]
    credit: Decimal,
    currency: Option<String>,
    fx_rate: Option<Decimal>,
    memo: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    account_id: Uuid,
    debit: Decimal,
    credit: Decimal,
    currency: Option<String>,
    fx_rate: Option<Decimal>,
    memo: Option<String>,
}

async fn load_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Line>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, account_id, debit, credit, currency, fx_rate, memo
         FROM journal_entry_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

struct JournalEntry;

#[async_trait]
impl DocumentHandler for JournalEntry {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: JOURNAL_ENTRY.into(),
            module: String::new(),
            label: "Journal entry".into(),
            number_prefix: "JE".into(),
            fields: vec![],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("account_id", "Account", FieldType::link("account")).required(),
                    FieldMeta::new("debit", "Debit", FieldType::Decimal),
                    FieldMeta::new("credit", "Credit", FieldType::Decimal),
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    ),
                    FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal),
                    FieldMeta::new("memo", "Memo", FieldType::Text),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("posted", "Posted"),
                StateMeta::locked("reversed", "Reversed"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "posted".into()),
                ("posted".into(), "reversed".into()),
            ],
        }
    }

    /// A draft may be incomplete or out of balance; it is checked in full
    /// when it is posted.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid journal entry body: {e}")))?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.debit.is_sign_negative() || line.credit.is_sign_negative() {
                return Err(Error::validation(format!(
                    "line {n}: amounts cannot be negative"
                )));
            }
            if !line.debit.is_zero() && !line.credit.is_zero() {
                return Err(Error::validation(format!(
                    "line {n}: give an amount on one side only, debit or credit"
                )));
            }
            // Finer than the ledger stores: refuse it here, or the draft
            // would keep a rounded rate without saying so.
            if line
                .fx_rate
                .is_some_and(|rate| rate.normalize().scale() > 10)
            {
                return Err(Error::validation(format!(
                    "line {n}: an exchange rate has at most 10 decimal places"
                )));
            }
        }

        let mut accounts: Vec<Uuid> = body.lines.iter().map(|l| l.account_id).collect();
        accounts.sort();
        accounts.dedup();
        let known: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM account WHERE id = ANY($1) AND company_id = $2",
        )
        .bind(&accounts)
        .bind(doc.company_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        if known != accounts.len() as i64 {
            return Err(Error::validation(
                "every line needs an account of the document's company",
            ));
        }

        sqlx::query("DELETE FROM journal_entry_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO journal_entry_line (id, document_id, line_no, account_id, debit, credit, currency, fx_rate, memo)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.account_id)
            .bind(line.debit)
            .bind(line.credit)
            .bind(&line.currency)
            .bind(line.fx_rate)
            .bind(&line.memo)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        Ok(json!({ "lines": load_lines(conn, doc.id).await? }))
    }
}

// ----------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PostInput {
    /// Journal entry id.
    id: Uuid,
}

struct PostJournalEntry;

#[async_trait]
impl Command for PostJournalEntry {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "journal_entry.post",
            "Post a draft journal entry to the general ledger. It receives its number and can no \
             longer be changed. Posting an entry that is already posted returns it unchanged.",
        )
        .permission("journal_entry:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, JOURNAL_ENTRY, input.id, "post").await?;
        // Whoever gets the row lock second finds the entry posted and
        // receives the same result as the first.
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::conflict(format!(
                "a {} journal entry cannot be posted",
                doc.status
            )));
        }

        let lines = load_lines(&mut *ctx.conn, doc.id).await?;
        if lines.len() < 2 {
            return Err(Error::validation(
                "a journal entry needs at least two lines",
            ));
        }
        let entries: Vec<GlLine> = lines
            .iter()
            .map(|line| GlLine {
                account_id: line.account_id,
                debit: line.debit,
                credit: line.credit,
                currency: line.currency.clone(),
                fx_rate: line.fx_rate,
                memo: line.memo.clone(),
                source_line_id: Some(line.id),
            })
            .collect();

        document::assign_number(ctx, &mut doc).await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        erpcraft_gl::post(&mut *ctx.conn, &posting, &entries).await?;
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReverseInput {
    /// Journal entry id.
    id: Uuid,
    /// Accounting date of the reversal. Defaults to the date of the
    /// original posting, whose period must then still be open.
    posting_date: Option<NaiveDate>,
    memo: Option<String>,
}

struct ReverseJournalEntry;

#[async_trait]
impl Command for ReverseJournalEntry {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "journal_entry.reverse",
            "Cancel a posted journal entry by adding a second posting with debit and credit swapped. \
             The original posting is kept unchanged.",
        )
        .permission("journal_entry:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, JOURNAL_ENTRY, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::conflict(format!(
                "a {} journal entry cannot be reversed",
                doc.status
            )));
        }
        let original = document::postings_for(&mut *ctx.conn, doc.id)
            .await?
            .into_iter()
            .find(|p| p.reverses_id.is_none())
            .ok_or_else(|| Error::internal("a posted journal entry has no posting"))?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
