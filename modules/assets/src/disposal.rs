//! Selling or scrapping an asset: depreciated up to the day, then taken out
//! of the books, the proceeds booked and the gain or loss with them.

use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType, JsonSchema,
    PgConnection, Result, StateMeta, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::plan::month_end;
use crate::{
    ACCUMULATED, ASSET_COST, EXPENSE, GAIN, LOSS, account, load_asset, settings, status_of,
};

pub(crate) const ASSET_DISPOSAL: &str = "asset_disposal";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    asset_id: Uuid,
    #[serde(default)]
    proceeds: Decimal,
    proceeds_account_id: Option<Uuid>,
    // What a posted disposal came to: shown, not written.
    #[serde(default)]
    #[allow(dead_code)]
    depreciation: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    cost: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    accumulated: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    book_value: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    gain: Option<Value>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Header {
    asset_id: Uuid,
    proceeds: Decimal,
    proceeds_account_id: Option<Uuid>,
    depreciation: Option<Decimal>,
    cost: Option<Decimal>,
    accumulated: Option<Decimal>,
    book_value: Option<Decimal>,
    gain: Option<Decimal>,
}

async fn load_header(conn: &mut PgConnection, document_id: Uuid) -> Result<Header> {
    sqlx::query_as(
        "SELECT asset_id, proceeds, proceeds_account_id, depreciation, cost, accumulated,
                book_value, gain
         FROM asset_disposal WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found(ASSET_DISPOSAL))
}

pub(crate) struct Disposal;

#[async_trait]
impl DocumentHandler for Disposal {
    fn meta(&self) -> DocTypeMeta {
        let money = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::Decimal)
                .money()
                .readonly()
        };
        DocTypeMeta {
            name: ASSET_DISPOSAL.into(),
            module: String::new(),
            label: "Asset disposal".into(),
            number_prefix: "ADS".into(),
            fields: vec![
                FieldMeta::new("asset_id", "Asset", FieldType::link("asset"))
                    .required()
                    .help("Sold or scrapped on the document date."),
                FieldMeta::new("proceeds", "Proceeds", FieldType::Decimal)
                    .money()
                    .help("What it was sold for, in the accounting currency; 0 when scrapped."),
                FieldMeta::new(
                    "proceeds_account_id",
                    "Proceeds to",
                    FieldType::link("account"),
                )
                .help(
                    "A bank account, or a clearing account that a sales invoice to the buyer \
                     settles. Needed when there are proceeds.",
                ),
                money("depreciation", "Depreciation to the date"),
                money("cost", "Cost removed"),
                money("accumulated", "Accumulated depreciation removed"),
                money("book_value", "Book value"),
                money("gain", "Gain (loss below zero)"),
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
                FieldMeta::new("asset_id", "Asset", FieldType::link("asset")).readonly(),
                FieldMeta::new("proceeds", "Proceeds", FieldType::Decimal)
                    .money()
                    .readonly(),
                FieldMeta::new("gain", "Gain (loss below zero)", FieldType::Decimal)
                    .money()
                    .readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "asset disposal body")?;
        let asset = load_asset(&mut *ctx.conn, body.asset_id, false)
            .await
            .map_err(|_| Error::field("asset_id", "must be an asset"))?;
        if asset.company_id != doc.company_id {
            return Err(Error::field(
                "asset_id",
                "must be an asset of the document's company",
            ));
        }
        if body.proceeds < Decimal::ZERO {
            return Err(Error::field("proceeds", "cannot be below zero"));
        }
        crate::check_places(&mut *ctx.conn, doc.company_id, "proceeds", body.proceeds).await?;
        if let Some(account_id) = body.proceeds_account_id {
            let ok: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM account WHERE id = $1 AND company_id = $2
                                AND NOT is_group)",
            )
            .bind(account_id)
            .bind(doc.company_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            if !ok {
                return Err(Error::field(
                    "proceeds_account_id",
                    "must be an account of the company that can be posted to",
                ));
            }
        } else if !body.proceeds.is_zero() {
            return Err(Error::field(
                "proceeds_account_id",
                "is needed: say where the proceeds go",
            ));
        }
        sqlx::query(
            "INSERT INTO asset_disposal (document_id, asset_id, proceeds, proceeds_account_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (document_id) DO UPDATE SET asset_id = EXCLUDED.asset_id,
                 proceeds = EXCLUDED.proceeds, proceeds_account_id = EXCLUDED.proceeds_account_id",
        )
        .bind(doc.id)
        .bind(body.asset_id)
        .bind(body.proceeds)
        .bind(body.proceeds_account_id)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        match load_header(&mut *conn, doc.id).await {
            Ok(header) => serde_json::to_value(header).map_err(Error::internal),
            Err(_) => Ok(json!({})),
        }
    }

    async fn summary(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let Ok(header) = load_header(&mut *conn, doc.id).await else {
            return Ok(json!({}));
        };
        Ok(json!({ "asset_id": header.asset_id, "proceeds": header.proceeds, "gain": header.gain }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdInput {
    /// Asset disposal id.
    id: Uuid,
}

pub(crate) struct Post;

#[async_trait]
impl Command for Post {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "asset_disposal.post",
            "Sell or scrap an asset on the document date: it is depreciated first for every \
             month that ended by then and was not yet, then its cost and accumulated \
             depreciation leave the books, the proceeds go to the account named, and the \
             difference to the book value is a gain (asset_disposal_gain) or a loss \
             (asset_disposal_loss). Posting a posted disposal returns it unchanged.",
        )
        .permission("asset_disposal:post")
        .label("Post")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, ASSET_DISPOSAL, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} asset disposal cannot be posted",
                doc.status
            )));
        }
        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let asset = load_asset(&mut *ctx.conn, header.asset_id, true).await?;
        ctx.touch("asset", asset.id);
        let date = doc.doc_date;
        if asset.status == "disposed" {
            return Err(Error::state(format!(
                "asset {} was disposed of on {}",
                asset.code,
                asset.disposed_on.map(|d| d.to_string()).unwrap_or_default()
            )));
        }
        let (Some(cost), Some(recorded_on)) = (asset.cost, asset.recorded_on) else {
            return Err(Error::state(format!(
                "asset {} is not capitalised: delete it instead",
                asset.code
            )));
        };
        if date < recorded_on {
            return Err(Error::field(
                "doc_date",
                format!("must not be before the asset is in the books, on {recorded_on}"),
            ));
        }
        let latest: Option<chrono::NaiveDate> = sqlx::query_scalar(
            "SELECT max(month) FROM asset_depreciation WHERE asset_id = $1 AND NOT reversed",
        )
        .bind(asset.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        if let Some(month) = latest
            && month_end(month) > date
        {
            return Err(Error::field(
                "doc_date",
                format!(
                    "is before {}, up to which the asset is depreciated: reverse that run first, \
                     or dispose of it later",
                    month_end(month)
                ),
            ));
        }
        let settings = settings(&mut *ctx.conn, asset.company_id).await?;
        let places =
            erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, asset.company_id).await?;
        let months = crate::due(&mut *ctx.conn, &asset, &settings, places, date).await?;
        let depreciation: Decimal = months.iter().map(|(_, a)| *a).sum();
        let before: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(sum(amount), 0) FROM asset_depreciation
             WHERE asset_id = $1 AND NOT reversed",
        )
        .bind(asset.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let accumulated = asset.opening_accumulated + before + depreciation;
        let book_value = cost - accumulated;
        let gain = header.proceeds - book_value;

        let (company, class) = (asset.company_id, asset.class_id);
        let cost_account = account(&mut *ctx.conn, company, ASSET_COST, class).await?;
        let accumulated_account = account(&mut *ctx.conn, company, ACCUMULATED, class).await?;
        let memo = Some(format!("Disposal of {} {}", asset.code, asset.name));
        let line = |account_id: Uuid, debit: Decimal, credit: Decimal| GlLine {
            account_id,
            debit,
            credit,
            currency: None,
            fx_rate: None,
            memo: memo.clone(),
            source_line_id: None,
        };
        let mut gl = Vec::new();
        if depreciation > Decimal::ZERO {
            let expense = account(&mut *ctx.conn, company, EXPENSE, class).await?;
            gl.push(line(expense, depreciation, Decimal::ZERO));
            gl.push(line(accumulated_account, Decimal::ZERO, depreciation));
        }
        if accumulated > Decimal::ZERO {
            gl.push(line(accumulated_account, accumulated, Decimal::ZERO));
        }
        gl.push(line(cost_account, Decimal::ZERO, cost));
        if header.proceeds > Decimal::ZERO {
            let to = header
                .proceeds_account_id
                .ok_or_else(|| Error::internal("proceeds without an account"))?;
            gl.push(line(to, header.proceeds, Decimal::ZERO));
        }
        if gain > Decimal::ZERO {
            let to = account(&mut *ctx.conn, company, GAIN, class).await?;
            gl.push(line(to, Decimal::ZERO, gain));
        } else if gain < Decimal::ZERO {
            let to = account(&mut *ctx.conn, company, LOSS, class).await?;
            gl.push(line(to, -gain, Decimal::ZERO));
        }
        let posting = document::create_posting(ctx, &doc, date, memo.clone()).await?;
        erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        crate::run::record(&mut *ctx.conn, &asset, doc.id, date, &months).await?;
        sqlx::query(
            "UPDATE asset_disposal SET depreciation = $2, cost = $3, accumulated = $4,
                 book_value = $5, gain = $6
             WHERE document_id = $1",
        )
        .bind(doc.id)
        .bind(depreciation)
        .bind(cost)
        .bind(accumulated)
        .bind(book_value)
        .bind(gain)
        .execute(&mut *ctx.conn)
        .await?;
        sqlx::query(
            "UPDATE asset SET status = 'disposed', disposed_on = $2, disposal_id = $3 WHERE id = $1",
        )
        .bind(asset.id)
        .bind(date)
        .bind(doc.id)
        .execute(&mut *ctx.conn)
        .await?;
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

pub(crate) struct Reverse;

#[async_trait]
impl Command for Reverse {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "asset_disposal.reverse",
            "Reverse a posted disposal exactly, on its own date: the asset is back in the \
             books as it was, with the depreciation the disposal took undone.",
        )
        .permission("asset_disposal:reverse")
        .label("Reverse")
        .from_states(&["posted"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, ASSET_DISPOSAL, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} asset disposal cannot be reversed",
                doc.status
            )));
        }
        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let asset = load_asset(&mut *ctx.conn, header.asset_id, true).await?;
        ctx.touch("asset", asset.id);
        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = doc.number.as_ref().map(|n| format!("Reversal of {n}"));
        document::reverse_posting(ctx, original.id, None, memo).await?;
        sqlx::query("UPDATE asset_depreciation SET reversed = true WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        let settings = settings(&mut *ctx.conn, asset.company_id).await?;
        let places =
            erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, asset.company_id).await?;
        let status = status_of(&mut *ctx.conn, &asset, &settings, places).await?;
        sqlx::query(
            "UPDATE asset SET status = $2, disposed_on = NULL, disposal_id = NULL WHERE id = $1",
        )
        .bind(asset.id)
        .bind(status)
        .execute(&mut *ctx.conn)
        .await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
