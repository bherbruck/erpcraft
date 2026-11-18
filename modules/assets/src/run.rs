//! The depreciation run: every asset in service, depreciated up to a month
//! end and posted at once.

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType,
    JsonSchema, PgConnection, Result, StateMeta, async_trait, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::plan::month_end;
use crate::{ACCUMULATED, ASSET_COLUMNS, Asset, EXPENSE, account, settings, status_of};

pub(crate) const DEPRECIATION_RUN: &str = "depreciation_run";

/// What a run depreciates of one asset.
struct Due {
    asset: Asset,
    months: Vec<(NaiveDate, Decimal)>,
}

impl Due {
    fn amount(&self) -> Decimal {
        self.months.iter().map(|(_, a)| *a).sum()
    }
}

/// Every asset of the company in service by `period_end` with months to
/// depreciate up to it, in the order of their codes.
async fn dues(
    conn: &mut PgConnection,
    company_id: Uuid,
    period_end: NaiveDate,
    lock: bool,
) -> Result<Vec<Due>> {
    let settings = settings(&mut *conn, company_id).await?;
    let places = erpcraft_kernel::company::currency_decimals(&mut *conn, company_id).await?;
    let sql = format!(
        "SELECT {ASSET_COLUMNS} FROM asset
         WHERE company_id = $1 AND status = 'in_service' AND in_service_date <= $2
           AND method <> 'none'
         ORDER BY code{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let assets: Vec<Asset> = sqlx::query_as(&sql)
        .bind(company_id)
        .bind(period_end)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = Vec::new();
    for asset in assets {
        let months = crate::due(&mut *conn, &asset, &settings, places, period_end).await?;
        if !months.is_empty() {
            out.push(Due { asset, months });
        }
    }
    Ok(out)
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Line {
    id: Option<Uuid>,
    line_no: i32,
    asset_id: Uuid,
    first_month: NaiveDate,
    last_month: NaiveDate,
    months: i32,
    amount: Decimal,
    expense_account_id: Option<Uuid>,
    accumulated_account_id: Option<Uuid>,
}

async fn stored_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, asset_id, first_month, last_month, months, amount,
                expense_account_id, accumulated_account_id
         FROM depreciation_run_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The lines a draft would post if it were posted now. An account that is
/// not set up yet shows as none; posting says what is missing.
async fn proposed(conn: &mut PgConnection, doc: &Document) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    if month_end(doc.doc_date) != doc.doc_date {
        return Ok(lines);
    }
    for (index, due) in dues(&mut *conn, doc.company_id, doc.doc_date, false)
        .await?
        .into_iter()
        .enumerate()
    {
        let (company, class) = (due.asset.company_id, due.asset.class_id);
        lines.push(Line {
            id: None,
            line_no: index as i32 + 1,
            asset_id: due.asset.id,
            first_month: due.months[0].0,
            last_month: due.months[due.months.len() - 1].0,
            months: due.months.len() as i32,
            amount: due.amount(),
            expense_account_id: account(&mut *conn, company, EXPENSE, class).await.ok(),
            accumulated_account_id: account(&mut *conn, company, ACCUMULATED, class).await.ok(),
        });
    }
    Ok(lines)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    /// Shown, not written: the run's date is its period end, and its lines
    /// are what it depreciates.
    #[serde(default)]
    #[allow(dead_code)]
    period_end: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    lines: Option<Value>,
}

pub(crate) struct DepreciationRun;

#[async_trait]
impl DocumentHandler for DepreciationRun {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: DEPRECIATION_RUN.into(),
            module: String::new(),
            label: "Depreciation run".into(),
            number_prefix: "DEP".into(),
            fields: vec![
                FieldMeta::new("period_end", "Period end", FieldType::Date)
                    .readonly()
                    .help(
                        "The document date, which must be the last day of a month: every asset \
                         in service is depreciated up to it.",
                    ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Assets".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("asset_id", "Asset", FieldType::link("asset")).readonly(),
                    FieldMeta::new("first_month", "From month", FieldType::Date).readonly(),
                    FieldMeta::new("last_month", "To month", FieldType::Date).readonly(),
                    FieldMeta::new("months", "Months", FieldType::Int).readonly(),
                    FieldMeta::new("amount", "Depreciation", FieldType::Decimal)
                        .money()
                        .readonly(),
                    FieldMeta::new(
                        "expense_account_id",
                        "Expense account",
                        FieldType::link("account"),
                    )
                    .readonly(),
                    FieldMeta::new(
                        "accumulated_account_id",
                        "Accumulated depreciation account",
                        FieldType::link("account"),
                    )
                    .readonly(),
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
                FieldMeta::new("period_end", "Period end", FieldType::Date).readonly(),
                FieldMeta::new("assets", "Assets", FieldType::Int).readonly(),
                FieldMeta::new("amount", "Depreciation", FieldType::Decimal)
                    .money()
                    .readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, _ctx: &mut Ctx<'_>, _doc: &Document, body: &Value) -> Result<()> {
        let _: Body = erpcraft_kernel::input::parse(body.clone(), "depreciation run body")?;
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let lines = if doc.status == "draft" {
            proposed(&mut *conn, doc).await?
        } else {
            stored_lines(&mut *conn, doc.id).await?
        };
        Ok(json!({ "period_end": doc.doc_date, "lines": lines }))
    }

    async fn summary(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        if doc.status == "draft" {
            return Ok(json!({ "period_end": doc.doc_date }));
        }
        let (assets, amount): (i64, Option<Decimal>) = sqlx::query_as(
            "SELECT count(*), sum(amount) FROM depreciation_run_line WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        Ok(
            json!({ "period_end": doc.doc_date, "assets": assets, "amount": amount.unwrap_or_default() }),
        )
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdInput {
    /// Depreciation run id.
    id: Uuid,
}

pub(crate) struct Post;

#[async_trait]
impl Command for Post {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "depreciation_run.post",
            "Depreciate every asset of the company that is in service up to the run's date, the \
             last day of a month, and post it: for each asset the months since it was last \
             depreciated, never below its salvage value, debiting its class's \
             depreciation_expense account and crediting accumulated_depreciation. Runs go \
             forward: a run for a period that was run, or before one, is refused. Posting a \
             posted run returns it unchanged.",
        )
        .permission("depreciation_run:post")
        .label("Post")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, DEPRECIATION_RUN, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} depreciation run cannot be posted",
                doc.status
            )));
        }
        let period_end = doc.doc_date;
        if month_end(period_end) != period_end {
            return Err(Error::field(
                "doc_date",
                format!(
                    "must be the last day of a month ({}): a run depreciates whole months",
                    month_end(period_end)
                ),
            ));
        }
        let later: Option<(NaiveDate, Option<String>)> = sqlx::query_as(
            "SELECT p.period_end, d.number FROM depreciation_period p
             JOIN document d ON d.id = p.document_id
             WHERE p.company_id = $1 AND p.period_end >= $2
             ORDER BY p.period_end DESC LIMIT 1",
        )
        .bind(doc.company_id)
        .bind(period_end)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some((end, number)) = later {
            let which = number.unwrap_or_default();
            let why = if end == period_end {
                format!("{which} has depreciated {period_end} already; nothing is left to post")
            } else {
                format!("{which} has depreciated up to {end}; a run goes after the last one")
            };
            return Err(Error::state(why));
        }
        // The database refuses it too, and makes concurrent runs wait.
        sqlx::query(
            "INSERT INTO depreciation_period (company_id, period_end, document_id)
             VALUES ($1, $2, $3)",
        )
        .bind(doc.company_id)
        .bind(period_end)
        .bind(doc.id)
        .execute(&mut *ctx.conn)
        .await?;

        let dues = dues(&mut *ctx.conn, doc.company_id, period_end, true).await?;
        if !dues.is_empty() {
            let posting = document::create_posting(
                ctx,
                &doc,
                period_end,
                Some(format!("Depreciation to {period_end}")),
            )
            .await?;
            let mut gl = Vec::new();
            for (index, due) in dues.iter().enumerate() {
                let asset = &due.asset;
                let n = index + 1;
                let expense = account(&mut *ctx.conn, asset.company_id, EXPENSE, asset.class_id)
                    .await
                    .map_err(|e| e.with_line("lines", n))?;
                let accumulated = account(
                    &mut *ctx.conn,
                    asset.company_id,
                    ACCUMULATED,
                    asset.class_id,
                )
                .await
                .map_err(|e| e.with_line("lines", n))?;
                let line_id = new_id();
                let amount = due.amount();
                sqlx::query(
                    "INSERT INTO depreciation_run_line (id, document_id, line_no, asset_id,
                         first_month, last_month, months, amount, expense_account_id,
                         accumulated_account_id)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                )
                .bind(line_id)
                .bind(doc.id)
                .bind(n as i32)
                .bind(asset.id)
                .bind(due.months[0].0)
                .bind(due.months[due.months.len() - 1].0)
                .bind(due.months.len() as i32)
                .bind(amount)
                .bind(expense)
                .bind(accumulated)
                .execute(&mut *ctx.conn)
                .await?;
                record(&mut *ctx.conn, asset, doc.id, period_end, &due.months).await?;
                let memo = Some(format!("{} {}", asset.code, asset.name));
                gl.push(GlLine {
                    account_id: expense,
                    debit: amount,
                    credit: Decimal::ZERO,
                    currency: None,
                    fx_rate: None,
                    memo: memo.clone(),
                    source_line_id: Some(line_id),
                    scopes: vec![("asset", asset.id), ("asset_class", asset.class_id)],
                    ..GlLine::default()
                });
                gl.push(GlLine {
                    account_id: accumulated,
                    debit: Decimal::ZERO,
                    credit: amount,
                    currency: None,
                    fx_rate: None,
                    memo,
                    source_line_id: Some(line_id),
                    scopes: vec![("asset", asset.id), ("asset_class", asset.class_id)],
                    ..GlLine::default()
                });
            }
            erpcraft_gl::post(ctx, &posting, &gl).await?;
            refresh_status(ctx, dues.iter().map(|d| d.asset.id)).await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

/// Record the months depreciated: each once, which the database enforces.
pub(crate) async fn record(
    conn: &mut PgConnection,
    asset: &Asset,
    document_id: Uuid,
    posting_date: NaiveDate,
    months: &[(NaiveDate, Decimal)],
) -> Result<()> {
    let ids: Vec<Uuid> = months.iter().map(|_| new_id()).collect();
    let dates: Vec<NaiveDate> = months.iter().map(|(m, _)| *m).collect();
    let amounts: Vec<Decimal> = months.iter().map(|(_, a)| *a).collect();
    sqlx::query(
        "INSERT INTO asset_depreciation (id, company_id, asset_id, document_id, month,
             posting_date, amount)
         SELECT r.id, $1, $2, $3, r.month, $4, r.amount
         FROM unnest($5::uuid[], $6::date[], $7::numeric[]) AS r(id, month, amount)",
    )
    .bind(asset.company_id)
    .bind(asset.id)
    .bind(document_id)
    .bind(posting_date)
    .bind(&ids)
    .bind(&dates)
    .bind(&amounts)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Set the status of assets not disposed of from their depreciation.
pub(crate) async fn refresh_status(
    ctx: &mut Ctx<'_>,
    assets: impl Iterator<Item = Uuid>,
) -> Result<()> {
    for id in assets {
        let asset = crate::load_asset(&mut *ctx.conn, id, true).await?;
        if asset.status == "disposed" {
            continue;
        }
        let settings = settings(&mut *ctx.conn, asset.company_id).await?;
        let places =
            erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, asset.company_id).await?;
        let status = status_of(&mut *ctx.conn, &asset, &settings, places).await?;
        if status != asset.status {
            sqlx::query("UPDATE asset SET status = $2 WHERE id = $1")
                .bind(id)
                .bind(status)
                .execute(&mut *ctx.conn)
                .await?;
        }
        ctx.touch("asset", id);
    }
    Ok(())
}

pub(crate) struct Reverse;

#[async_trait]
impl Command for Reverse {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "depreciation_run.reverse",
            "Reverse a posted depreciation run exactly, on its own date: its posting is \
             reversed and its months are depreciated again by the next run. Only the company's \
             latest run can be reversed, and not while an asset it depreciated is disposed of.",
        )
        .permission("depreciation_run:reverse")
        .label("Reverse")
        .from_states(&["posted"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, DEPRECIATION_RUN, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} depreciation run cannot be reversed",
                doc.status
            )));
        }
        let latest: Option<(Uuid, Option<String>)> = sqlx::query_as(
            "SELECT p.document_id, d.number FROM depreciation_period p
             JOIN document d ON d.id = p.document_id
             WHERE p.company_id = $1 ORDER BY p.period_end DESC LIMIT 1",
        )
        .bind(doc.company_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some((latest, number)) = latest
            && latest != doc.id
        {
            return Err(Error::state(format!(
                "{} is a later run: reverse it first",
                number.unwrap_or_default()
            )));
        }
        let lines = stored_lines(&mut *ctx.conn, doc.id).await?;
        let assets: Vec<Uuid> = lines.iter().map(|l| l.asset_id).collect();
        let disposed: Option<String> = sqlx::query_scalar(
            "SELECT code FROM asset WHERE id = ANY($1) AND status = 'disposed' ORDER BY code LIMIT 1",
        )
        .bind(&assets)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        if let Some(code) = disposed {
            return Err(Error::state(format!(
                "asset {code} has been disposed of since: reverse its disposal first"
            )));
        }
        if let Some(original) = document::postings_for(&mut *ctx.conn, doc.id)
            .await?
            .into_iter()
            .find(|p| p.reverses_id.is_none())
        {
            let memo = doc.number.as_ref().map(|n| format!("Reversal of {n}"));
            document::reverse_posting(ctx, original.id, None, memo).await?;
        }
        sqlx::query("UPDATE asset_depreciation SET reversed = true WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        sqlx::query("DELETE FROM depreciation_period WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        refresh_status(ctx, assets.into_iter()).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
