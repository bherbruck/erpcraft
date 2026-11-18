//! Accounting module. Uses only the public module API of the kernel and
//! the general ledger, exactly as a module outside this repository would.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::{Dimensions, GlLine};
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, PgConnection, Result, StateMeta, async_trait,
    command, company, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod year_end;

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
        vec![
            Migration {
                version: 1,
                name: "accounting",
                sql: include_str!("../migrations/0001_accounting.sql"),
            },
            Migration {
                version: 2,
                name: "year_end_close",
                sql: include_str!("../migrations/0002_year_end_close.sql"),
            },
            Migration {
                version: 3,
                name: "year_of_periods",
                sql: include_str!("../migrations/0003_year_of_periods.sql"),
            },
            Migration {
                version: 4,
                name: "rate_precision",
                sql: include_str!("../migrations/0004_rate_precision.sql"),
            },
            Migration {
                version: 5,
                name: "line_dimensions",
                sql: include_str!("../migrations/0005_line_dimensions.sql"),
            },
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(JournalEntry), Arc::new(year_end::YearEndClose)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(PostJournalEntry),
            command(ReverseJournalEntry),
            command(year_end::PostYearEndClose),
            command(year_end::ReverseYearEndClose),
        ]
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
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    account_id: Uuid,
    #[serde(default)]
    debit: Decimal,
    #[serde(default)]
    credit: Decimal,
    currency: Option<String>,
    fx_rate: Option<Decimal>,
    memo: Option<String>,
    #[serde(default)]
    dimension_value_ids: Vec<Uuid>,
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
    dimension_value_ids: Vec<Uuid>,
}

async fn load_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Line>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, account_id, debit, credit, currency, fx_rate, memo,
                dimension_value_ids
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
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("account_id", "Account", FieldType::link("account")).required(),
                    FieldMeta::new("debit", "Debit", FieldType::Decimal)
                        .money()
                        .currency_in("currency"),
                    FieldMeta::new("credit", "Credit", FieldType::Decimal)
                        .money()
                        .currency_in("currency"),
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    ),
                    FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
                        .rate(erpcraft_gl::RATE_PLACES),
                    FieldMeta::new("memo", "Memo", FieldType::Text),
                    FieldMeta::new("dimension_value_ids", "Dimensions", FieldType::TextArray).help(
                        "Ids of dimension values, at most one per dimension. The company's \
                             other dimensions take their defaults when the entry is posted.",
                    ),
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
            summary: vec![
                FieldMeta::new("total_debit", "Total", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help(
                        "The debit side in the accounting currency, which equals the credit side.",
                    ),
            ],
            ..DocTypeMeta::default()
        }
    }

    /// A draft may be incomplete or out of balance; it is checked in full
    /// when it is posted.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "journal entry body")?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.debit.is_sign_negative() || line.credit.is_sign_negative() {
                let field = if line.debit.is_sign_negative() {
                    "debit"
                } else {
                    "credit"
                };
                return Err(Error::line(
                    "lines",
                    n,
                    Some(field),
                    "amounts cannot be negative",
                ));
            }
            if !line.debit.is_zero() && !line.credit.is_zero() {
                return Err(Error::line(
                    "lines",
                    n,
                    None,
                    "give an amount on one side only, debit or credit",
                ));
            }
            // Finer than the ledger stores: refuse it here, or the draft
            // would keep a rounded rate without saying so.
            if line
                .fx_rate
                .is_some_and(|rate| rate.normalize().scale() > erpcraft_gl::RATE_PLACES)
            {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("fx_rate"),
                    "an exchange rate has at most 18 decimal places",
                ));
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

        for (index, line) in body.lines.iter().enumerate() {
            if let Some(refusal) = erpcraft_gl::values_refusal(
                &mut *ctx.conn,
                doc.company_id,
                &line.dimension_value_ids,
            )
            .await?
            {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("dimension_value_ids"),
                    &refusal,
                ));
            }
        }

        let ids = document::line_ids(
            &mut *ctx.conn,
            "journal_entry_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM journal_entry_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        if body.lines.is_empty() {
            return Ok(());
        }
        // One statement for all lines.
        let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
        let accounts: Vec<Uuid> = body.lines.iter().map(|l| l.account_id).collect();
        let debits: Vec<Decimal> = body.lines.iter().map(|l| l.debit).collect();
        let credits: Vec<Decimal> = body.lines.iter().map(|l| l.credit).collect();
        let currencies: Vec<Option<&str>> =
            body.lines.iter().map(|l| l.currency.as_deref()).collect();
        let rates: Vec<Option<Decimal>> = body.lines.iter().map(|l| l.fx_rate).collect();
        let memos: Vec<Option<&str>> = body.lines.iter().map(|l| l.memo.as_deref()).collect();
        // A list per line, sent as text since Postgres has no array of
        // arrays of different lengths.
        let dimensions: Vec<String> = body
            .lines
            .iter()
            .map(|l| {
                let ids: Vec<String> = l.dimension_value_ids.iter().map(Uuid::to_string).collect();
                format!("{{{}}}", ids.join(","))
            })
            .collect();
        sqlx::query(
            "INSERT INTO journal_entry_line (id, document_id, line_no, account_id, debit, credit,
                 currency, fx_rate, memo, dimension_value_ids)
             SELECT r.id, $1, r.line_no, r.account_id, r.debit, r.credit, r.currency, r.fx_rate,
                 r.memo, r.dimensions::uuid[]
             FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[], $6::numeric[],
                         $7::text[], $8::numeric[], $9::text[], $10::text[])
                AS r(id, line_no, account_id, debit, credit, currency, fx_rate, memo, dimensions)
             ORDER BY r.line_no",
        )
        .bind(doc.id)
        .bind(&ids)
        .bind(&line_nos)
        .bind(&accounts)
        .bind(&debits)
        .bind(&credits)
        .bind(&currencies)
        .bind(&rates)
        .bind(&memos)
        .bind(&dimensions)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    /// The total of the debit side in the accounting currency. Once
    /// posted it is what the ledger holds. For a draft it is worked out as
    /// the ledger will, and unknown while a line in another currency has
    /// no rate of its own: the rate on record is taken only at posting.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        use erpcraft_kernel::document::body::{amount, decimal, lines};
        let total: Option<Decimal> = if doc.status == "draft" {
            let base = company::base_currency(conn, doc.company_id).await?;
            let decimals = company::currency_decimals(conn, doc.company_id).await?;
            let mut total = Some(Decimal::ZERO);
            for line in lines(body, "lines") {
                let rate = match line["currency"].as_str() {
                    Some(currency) if *currency != base => decimal(&line["fx_rate"]),
                    _ => Some(Decimal::ONE),
                };
                total = match (total, rate) {
                    (Some(total), Some(rate)) => {
                        Some(total + round_money(amount(&line["debit"]) * rate, decimals))
                    }
                    _ => None,
                };
            }
            total
        } else {
            sqlx::query_scalar(
                "SELECT sum(g.debit) FROM gl_entry g JOIN posting p ON p.id = g.posting_id
                 WHERE p.document_id = $1 AND p.reverses_id IS NULL",
            )
            .bind(doc.id)
            .fetch_one(&mut *conn)
            .await?
        };
        Ok(json!({ "total_debit": total }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        Ok(json!({ "lines": load_lines(conn, doc.id).await? }))
    }
}

// ----------------------------------------------------------------- commands

/// Say which line of the journal entry a refusal of the general ledger is
/// about. The ledger numbers the entries it is given ("line 2: ..."), and
/// only the caller knows what they are; here entry n is line n.
fn at_line(error: Error) -> Error {
    let line = error
        .to_string()
        .strip_prefix("line ")
        .and_then(|rest| rest.split_once(':'))
        .and_then(|(number, _)| number.parse::<usize>().ok());
    match line {
        Some(line) => error.with_line("lines", line),
        None => error,
    }
}

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
        .from_states(&["draft"])
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
            return Err(Error::state(format!(
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
                dimensions: if line.dimension_value_ids.is_empty() {
                    Dimensions::Defaults
                } else {
                    Dimensions::Stated(line.dimension_value_ids.clone())
                },
                ..GlLine::default()
            })
            .collect();

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        erpcraft_gl::post(ctx, &posting, &entries)
            .await
            .map_err(at_line)?;
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
    /// Memo of the reversing posting. Defaults to "Reversal of" and the document number.
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
        .from_states(&["posted"])
        .destructive()
        .permission("journal_entry:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, JOURNAL_ENTRY, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
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
