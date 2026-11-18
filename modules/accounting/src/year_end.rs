//! Year-end close: brings the income and expense accounts of a fiscal
//! period to zero and moves the result to a retained earnings account.
//!
//! Convention shared with the general ledger's reports: the closing
//! posting is recognized by the command that created it. `posting.command`
//! is `year_end_close.post` for the close and `year_end_close.reverse` for
//! its reversal, and `gl.income_statement` leaves out every posting whose
//! command starts with `year_end_close.`. The commands of this document
//! must therefore keep that prefix, and no other command may use it.

use erpcraft_gl::{Dimensions, GlLine};
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType, JsonSchema,
    PgConnection, Result, StateMeta, async_trait, schemars,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

const YEAR_END_CLOSE: &str = "year_end_close";
/// The body field that names the retained earnings account.
const ACCOUNT: &str = "retained_earnings_account_id";

// ------------------------------------------------------------ document type

#[derive(Debug, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
struct Body {
    fiscal_period_id: Uuid,
    retained_earnings_account_id: Uuid,
    /// For a year kept as several periods: its first. The year then runs
    /// from the start of this period to the end of `fiscal_period_id`.
    #[serde(default)]
    first_period_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct Period {
    id: Uuid,
    name: String,
    start_date: chrono::NaiveDate,
    end_date: chrono::NaiveDate,
    status: String,
}

async fn load_body(conn: &mut PgConnection, document_id: Uuid) -> Result<Body> {
    sqlx::query_as(
        "SELECT fiscal_period_id, retained_earnings_account_id, first_period_id
         FROM year_end_close WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::internal("a year-end close has no content"))
}

/// Read the fiscal period of a year-end close and lock its row until the
/// transaction ends. Every ledger entry takes the row of its period
/// `FOR SHARE` (see `gl_entry_before_insert`), so `exclusive` keeps all
/// other postings into the period out, and waits for those in flight.
async fn lock_period(
    conn: &mut PgConnection,
    doc: &Document,
    period_id: Uuid,
    exclusive: bool,
) -> Result<Period> {
    let lock = if exclusive { "FOR UPDATE" } else { "FOR SHARE" };
    sqlx::query_as(&format!(
        "SELECT id, name, start_date, end_date, status
         FROM fiscal_period WHERE id = $1 AND company_id = $2 {lock}"
    ))
    .bind(period_id)
    .bind(doc.company_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| {
        Error::validation("the fiscal period must be a period of the document's company")
            .with_field("fiscal_period_id")
    })
}

/// The first period of the year a close covers: the one the body names,
/// else the closing period itself. It is one of the company's and starts
/// no later than the closing period.
async fn first_period(
    conn: &mut PgConnection,
    doc: &Document,
    body: &Body,
    last: &Period,
) -> Result<Period> {
    let Some(first) = body.first_period_id.filter(|id| *id != last.id) else {
        return Ok(Period {
            id: last.id,
            name: last.name.clone(),
            start_date: last.start_date,
            end_date: last.end_date,
            status: last.status.clone(),
        });
    };
    let first = lock_period(conn, doc, first, false)
        .await
        .map_err(|e| e.with_field("first_period_id"))?;
    if first.start_date > last.start_date {
        return Err(Error::validation(format!(
            "the year's first period, {}, starts after its last, {}",
            first.name, last.name
        ))
        .with_field("first_period_id"));
    }
    Ok(first)
}

/// The rules a year-end close must meet, as a draft and again when it is
/// posted: the date of a draft can change without its content being saved
/// again, and an account can change after the draft was written.
async fn check(
    conn: &mut PgConnection,
    doc: &Document,
    body: &Body,
    period: &Period,
) -> Result<()> {
    if doc.doc_date != period.end_date {
        return Err(Error::validation(format!(
            "a year-end close is dated the last day of its fiscal period: the date must be {}, \
             the last day of {}, not {}",
            period.end_date, period.name, doc.doc_date
        ))
        .with_field("doc_date"));
    }
    let account: Option<(String, String, bool, bool)> = sqlx::query_as(
        "SELECT code, account_type, is_group, active FROM account
         WHERE id = $1 AND company_id = $2",
    )
    .bind(body.retained_earnings_account_id)
    .bind(doc.company_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (code, account_type, is_group, active) = account.ok_or_else(|| {
        Error::validation(
            "the retained earnings account must be an account of the document's company",
        )
        .with_field(ACCOUNT)
    })?;
    if account_type != "equity" {
        return Err(Error::validation(format!(
            "the retained earnings account must be an equity account; {code} is an {account_type} account"
        ))
        .with_field(ACCOUNT));
    }
    if is_group {
        return Err(Error::validation(format!(
            "account {code} is a group account and cannot be posted to; \
             choose an equity account that is not a group"
        ))
        .with_field(ACCOUNT));
    }
    if !active {
        return Err(Error::validation(format!("account {code} is inactive")).with_field(ACCOUNT));
    }
    Ok(())
}

pub(crate) struct YearEndClose;

#[async_trait]
impl DocumentHandler for YearEndClose {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: YEAR_END_CLOSE.into(),
            module: String::new(),
            label: "Year-end close".into(),
            number_prefix: "YE".into(),
            fields: vec![
                FieldMeta::new(
                    "fiscal_period_id",
                    "Fiscal period",
                    FieldType::link("fiscal_period"),
                )
                .required(),
                FieldMeta::new(
                    "retained_earnings_account_id",
                    "Retained earnings account",
                    FieldType::link("account"),
                )
                .required(),
                FieldMeta::new(
                    "first_period_id",
                    "First period of the year",
                    FieldType::link("fiscal_period"),
                )
                .help(
                    "For a year kept as several periods (months): its first. The close then \
                     covers every period from this one to the fiscal period above, its last, \
                     and closes them all. Left out: the year is the fiscal period alone.",
                ),
                // Set by year_end_close.post and year_end_close.reverse.
                FieldMeta::new("closing_posting_id", "Closing posting", FieldType::Posting)
                    .readonly(),
                FieldMeta::new(
                    "reversal_posting_id",
                    "Reversal posting",
                    FieldType::Posting,
                )
                .readonly(),
            ],
            collections: vec![],
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
                FieldMeta::new(
                    "fiscal_period_id",
                    "Fiscal period",
                    FieldType::link("fiscal_period"),
                )
                .readonly(),
                FieldMeta::new("result", "Result of the year", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("What went to retained earnings: a profit, or a loss if negative."),
            ],
            ..DocTypeMeta::default()
        }
    }

    /// A draft is checked in full, except for what only the moment of
    /// posting can tell: whether the period is open, already closed by
    /// another document, or has anything to close.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "year-end close body")?;
        let period = lock_period(&mut *ctx.conn, doc, body.fiscal_period_id, false).await?;
        check(&mut *ctx.conn, doc, &body, &period).await?;
        first_period(&mut *ctx.conn, doc, &body, &period).await?;
        sqlx::query(
            "INSERT INTO year_end_close (document_id, fiscal_period_id, retained_earnings_account_id,
                 first_period_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (document_id) DO UPDATE SET
                fiscal_period_id = EXCLUDED.fiscal_period_id,
                retained_earnings_account_id = EXCLUDED.retained_earnings_account_id,
                first_period_id = EXCLUDED.first_period_id",
        )
        .bind(doc.id)
        .bind(body.fiscal_period_id)
        .bind(body.retained_earnings_account_id)
        .bind(body.first_period_id.filter(|id| *id != body.fiscal_period_id))
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    /// The period, and once posted the result that went to retained
    /// earnings: income minus expenses, negative for a loss.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let period = erpcraft_kernel::document::body::uuid(&body["fiscal_period_id"]);
        let result: Option<Decimal> = sqlx::query_scalar(
            "SELECT COALESCE(sum(g.credit) - sum(g.debit), 0)
             FROM year_end_closing c
             JOIN year_end_close y ON y.document_id = c.document_id
             LEFT JOIN gl_entry g ON g.posting_id = c.posting_id
                                 AND g.account_id = y.retained_earnings_account_id
             WHERE c.document_id = $1
             GROUP BY c.document_id",
        )
        .bind(doc.id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(json!({ "fiscal_period_id": period, "result": result }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        #[allow(clippy::type_complexity)]
        let row: Option<(Uuid, Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT y.fiscal_period_id, y.retained_earnings_account_id,
                    c.posting_id, c.reversal_posting_id, y.first_period_id
             FROM year_end_close y
             LEFT JOIN year_end_closing c ON c.document_id = y.document_id
             WHERE y.document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(match row {
            Some((period, account, posting, reversal, first)) => json!({
                "fiscal_period_id": period,
                "retained_earnings_account_id": account,
                "first_period_id": first,
                "closing_posting_id": posting,
                "reversal_posting_id": reversal,
            }),
            None => json!({}),
        })
    }
}

// ----------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostInput {
    /// Year-end close id.
    id: Uuid,
}

pub(crate) struct PostYearEndClose;

#[async_trait]
impl Command for PostYearEndClose {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "year_end_close.post",
            "Post a draft year-end close. In one posting dated the last day of the fiscal period, \
             every income and expense account of the company is brought to zero for the period, \
             and the result goes to the retained earnings account: a credit for a profit, a debit \
             for a loss. The amounts are worked out from the ledger at that moment. The command \
             then closes the fiscal period, as fiscal_period.close does, so nothing more can be \
             posted into the closed year; it needs no permission beyond year_end_close:post for \
             that. Refused if the period is closed, already has a year-end close that is not \
             reversed, or has nothing to close. Posting a close that is already posted returns it \
             unchanged. With first_period_id the year is every period from that one to the \
             fiscal period: their income and expenses are closed together, and all of them are \
             closed.",
        )
        .from_states(&["draft"])
        .permission("year_end_close:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, YEAR_END_CLOSE, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} year-end close cannot be posted",
                doc.status
            )));
        }

        let body = load_body(&mut *ctx.conn, doc.id).await?;
        // From here to the end of the command no one else posts into the
        // period, so the balances read below are the ones that are closed.
        let period = lock_period(&mut *ctx.conn, &doc, body.fiscal_period_id, true).await?;
        ctx.touch("fiscal_period", period.id);
        check(&mut *ctx.conn, &doc, &body, &period).await?;
        let first = first_period(&mut *ctx.conn, &doc, &body, &period).await?;
        // Every period of the year, earliest first, locked as the last is:
        // nothing else is posted into the year while it is closed.
        let year: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, status FROM fiscal_period
             WHERE company_id = $1 AND start_date >= $2 AND end_date <= $3
             ORDER BY start_date FOR UPDATE",
        )
        .bind(doc.company_id)
        .bind(first.start_date)
        .bind(period.end_date)
        .fetch_all(&mut *ctx.conn)
        .await?;

        // The unique index year_end_closing_one_per_period says the same.
        let in_effect: Option<Option<String>> = sqlx::query_scalar(
            "SELECT d.number FROM year_end_closing c
             JOIN document d ON d.id = c.document_id
             WHERE c.fiscal_period_id = $1 AND c.reversal_posting_id IS NULL",
        )
        .bind(period.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some(number) = in_effect {
            return Err(Error::duplicate(format!(
                "fiscal period {} already has a year-end close, {}; reverse that one before \
                 closing the period again",
                period.name,
                number.unwrap_or_default()
            )));
        }
        // A close covers the dates from its first period's start to its
        // last period's end. Two closes in effect never cover the same
        // date: the balances read below stop at this close's last day, so
        // they would not see a later close's entry, and income already
        // moved to retained earnings would be moved again. An earlier
        // close over any of these dates is reversed first.
        let overlapping: Option<(Option<String>, String, String)> = sqlx::query_as(
            "SELECT d.number, f.name, l.name
             FROM year_end_closing c
             JOIN document d ON d.id = c.document_id
             JOIN year_end_close y ON y.document_id = c.document_id
             JOIN fiscal_period l ON l.id = c.fiscal_period_id
             JOIN fiscal_period f ON f.id = COALESCE(y.first_period_id, c.fiscal_period_id)
             WHERE d.company_id = $1 AND c.reversal_posting_id IS NULL
               AND f.start_date <= $3 AND l.end_date >= $2
             ORDER BY l.end_date LIMIT 1",
        )
        .bind(doc.company_id)
        .bind(first.start_date)
        .bind(period.end_date)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some((number, from, to)) = overlapping {
            let covered = if from == to {
                from
            } else {
                format!("{from} to {to}")
            };
            return Err(Error::duplicate(format!(
                "the year-end close {} already closes fiscal period {covered}, which this close \
                 covers too; reverse that one (year_end_close.reverse) before closing these \
                 periods again",
                number.unwrap_or_default()
            )));
        }
        if period.status != "open" {
            return Err(Error::state(format!(
                "fiscal period {} is closed, so the year-end close cannot be posted into it; \
                 reopen it first with fiscal_period.reopen",
                period.name
            )));
        }

        // Debit minus credit of each income and expense account over the
        // period, per dimension set. Earlier closes of this period that were
        // reversed are in there twice with opposite signs, so they do not
        // count.
        let balances: Vec<(Uuid, Option<Uuid>, Decimal)> = sqlx::query_as(
            "SELECT a.id, g.dimension_set_id, sum(g.debit) - sum(g.credit)
             FROM account a
             JOIN gl_entry g ON g.account_id = a.id
             LEFT JOIN dimension_set ds ON ds.id = g.dimension_set_id
             WHERE a.company_id = $1 AND a.account_type IN ('income', 'expense')
               AND g.posting_date BETWEEN $2 AND $3
             GROUP BY a.id, g.dimension_set_id, ds.label
             HAVING sum(g.debit) <> sum(g.credit)
             ORDER BY a.code, ds.label NULLS FIRST",
        )
        .bind(doc.company_id)
        .bind(first.start_date)
        .bind(period.end_date)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let name = if first.id == period.id {
            period.name.clone()
        } else {
            format!("{} to {}", first.name, period.name)
        };
        if balances.is_empty() {
            return Err(Error::validation(format!(
                "there is nothing to close: no income or expense account has a balance in \
                 fiscal period {name}"
            )));
        }

        let memo = format!("Year-end close of {name}");
        // Every line keeps the dimension set it closes, so that the
        // result of each site, cost center or project goes to equity under
        // its own values.
        let line = |account_id: Uuid, set: Option<Uuid>, debit: Decimal, credit: Decimal| GlLine {
            account_id,
            debit,
            credit,
            currency: None,
            fx_rate: None,
            memo: Some(memo.clone()),
            source_line_id: None,
            dimensions: Dimensions::Exact(set),
            ..GlLine::default()
        };
        // Each account receives the opposite of its balance.
        let mut entries: Vec<GlLine> = balances
            .iter()
            .map(|&(account_id, set, balance)| {
                if balance.is_sign_positive() {
                    line(account_id, set, Decimal::ZERO, balance)
                } else {
                    line(account_id, set, -balance, Decimal::ZERO)
                }
            })
            .collect();
        // Income minus expenses, per dimension set. A set that breaks even
        // exactly has no entry for retained earnings.
        let mut results: Vec<(Option<Uuid>, Decimal)> = vec![];
        for &(_, set, balance) in &balances {
            match results.iter_mut().find(|(s, _)| *s == set) {
                Some((_, total)) => *total -= balance,
                None => results.push((set, -balance)),
            }
        }
        for (set, result) in results {
            if result > Decimal::ZERO {
                entries.push(line(
                    body.retained_earnings_account_id,
                    set,
                    Decimal::ZERO,
                    result,
                ));
            } else if result < Decimal::ZERO {
                entries.push(line(
                    body.retained_earnings_account_id,
                    set,
                    -result,
                    Decimal::ZERO,
                ));
            }
        }

        let posting_memo = doc.memo.clone().or_else(|| Some(memo.clone()));
        let posting = document::create_posting(ctx, &doc, period.end_date, posting_memo).await?;
        erpcraft_gl::post(ctx, &posting, &entries).await?;
        sqlx::query(
            "INSERT INTO year_end_closing (document_id, fiscal_period_id, posting_id)
             VALUES ($1, $2, $3)",
        )
        .bind(doc.id)
        .bind(period.id)
        .bind(posting.id)
        .execute(&mut *ctx.conn)
        .await?;
        // The whole year is closed: its last period, and every one before
        // it that is still open.
        let open: Vec<Uuid> = year
            .iter()
            .filter(|(_, status)| status == "open")
            .map(|(id, _)| *id)
            .chain([period.id])
            .collect();
        sqlx::query("UPDATE fiscal_period SET status = 'closed' WHERE id = ANY($1)")
            .bind(&open)
            .execute(&mut *ctx.conn)
            .await?;
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReverseInput {
    /// Year-end close id.
    id: Uuid,
    /// Memo of the reversing posting. Defaults to "Reversal of" and the document number.
    memo: Option<String>,
}

pub(crate) struct ReverseYearEndClose;

#[async_trait]
impl Command for ReverseYearEndClose {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "year_end_close.reverse",
            "Cancel a posted year-end close by adding a second posting with debit and credit \
             swapped, dated the last day of the fiscal period like the original, which is kept \
             unchanged. The income and expense accounts get their balances for the period back. \
             The fiscal period must be open: reopen it first with fiscal_period.reopen. It stays \
             open afterwards, and can be closed again with a new year-end close. Reversing a \
             close that is already reversed returns it unchanged.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("year_end_close:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, YEAR_END_CLOSE, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} year-end close cannot be reversed",
                doc.status
            )));
        }
        let closing: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT fiscal_period_id, posting_id FROM year_end_closing WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let (period_id, posting_id) =
            closing.ok_or_else(|| Error::internal("a posted year-end close has no posting"))?;

        // The ledger would refuse the entries of the reversal anyway; this
        // says what to do about it.
        let period = lock_period(&mut *ctx.conn, &doc, period_id, false).await?;
        if period.status != "open" {
            return Err(Error::state(format!(
                "fiscal period {} is closed, so the year-end close cannot be reversed; reopen \
                 it first with fiscal_period.reopen",
                period.name
            )));
        }

        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        let reversal = document::reverse_posting(ctx, posting_id, None, memo).await?;
        sqlx::query("UPDATE year_end_closing SET reversal_posting_id = $2 WHERE document_id = $1")
            .bind(doc.id)
            .bind(reversal.id)
            .execute(&mut *ctx.conn)
            .await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
