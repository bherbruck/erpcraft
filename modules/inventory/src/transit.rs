//! Transfers in two steps: goods shipped into a warehouse that holds goods
//! in transit, then received at the destination, in parts if they arrive
//! in parts.
//!
//! Shipping is the posting of a transfer that names a transit warehouse
//! (see `PostTransfer`): the goods leave the source at their average cost
//! and come into the transit warehouse with that value, carried on the
//! account for [`IN_TRANSIT_ACCOUNT`]. A transfer receipt takes them from
//! the transit warehouse into the destination at the transit warehouse's
//! average cost, which is what they came in for unless other shipments of
//! the same item wait there at another cost. How much of each line has
//! arrived is kept beside the transfer (`stock_transfer_progress`), so the
//! general ledger equals the stock ledger on every account at every step.

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType,
    JsonSchema, PgConnection, PolicyPoint, Result, StateMeta, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{PostInput, ReverseInput, STOCK_TRANSFER, TransferLine, load_transfer, move_stock};

/// The account purpose of goods in transit.
pub(crate) const IN_TRANSIT_ACCOUNT: &str = "inventory_in_transit";
/// The policy point asked for a transfer between sites in one step.
pub(crate) const BETWEEN_SITES: &str = "stock_transfer.between_sites";
const RECEIPT: &str = "stock_transfer_receipt";

pub(crate) fn between_sites() -> PolicyPoint {
    PolicyPoint::new(
        BETWEEN_SITES,
        "Asked when a stock transfer between warehouses at two different sites is posted in \
         one step, without a transit warehouse: may the goods arrive at the other site the \
         moment they leave? Unless configured otherwise they may not: goods between sites \
         travel, and a transfer names a transit warehouse to ship them through. A company \
         whose sites are next door allows it, for example for two given sites with \
         `facts.from_site == 'OH1' && facts.to_site == 'OH2'`.",
    )
    .text("from_site", "Code of the source warehouse's site")
    .text("to_site", "Code of the destination warehouse's site")
    .text("from_warehouse", "Code of the source warehouse")
    .text("to_warehouse", "Code of the destination warehouse")
    .refuse_by_default()
    .refusal(
        "a transfer between sites goes through a transit warehouse: name one in \
         transit_warehouse_id",
    )
}

/// Goods in transit leave a transit warehouse only with a transfer
/// receipt, and a transit warehouse is named only as the way through.
pub(crate) async fn check_warehouses(
    conn: &mut PgConnection,
    from: &Uuid,
    to: &Uuid,
    through: Option<Uuid>,
) -> Result<()> {
    let transit: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM warehouse WHERE id = ANY ($1) AND transit")
            .bind(vec![*from, *to])
            .fetch_all(&mut *conn)
            .await?;
    if transit.contains(from) || transit.contains(to) {
        return Err(Error::validation(
            "goods in transit come and go only through a transfer's transit warehouse: \
             name it in transit_warehouse_id, not as the source or the destination",
        )
        .with_field(if transit.contains(from) {
            "from_warehouse_id"
        } else {
            "to_warehouse_id"
        }));
    }
    if let Some(through) = through {
        let is_transit: bool = sqlx::query_scalar(
            "SELECT COALESCE(bool_or(transit), false) FROM warehouse WHERE id = $1",
        )
        .bind(through)
        .fetch_one(&mut *conn)
        .await?;
        if !is_transit {
            return Err(Error::validation(
                "the warehouse goods travel through must be one for goods in transit",
            )
            .with_field("transit_warehouse_id"));
        }
    }
    Ok(())
}

/// Ask [`BETWEEN_SITES`] when the two warehouses are at different sites.
pub(crate) async fn ask_between_sites(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    from: Uuid,
    to: Uuid,
) -> Result<()> {
    let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT w.id, w.code, s.code FROM warehouse w LEFT JOIN site s ON s.id = w.site_id
         WHERE w.id = ANY ($1)",
    )
    .bind(vec![from, to])
    .fetch_all(&mut *ctx.conn)
    .await?;
    let of = |id: Uuid| {
        rows.iter()
            .find(|(w, _, _)| *w == id)
            .map(|(_, code, site)| (code.clone(), site.clone()))
            .unwrap_or_default()
    };
    let ((from_code, from_site), (to_code, to_site)) = (of(from), of(to));
    let (Some(from_site), Some(to_site)) = (from_site, to_site) else {
        return Ok(());
    };
    if from_site == to_site {
        return Ok(());
    }
    ctx.decide(
        BETWEEN_SITES,
        company_id,
        json!({
            "from_site": from_site, "to_site": to_site,
            "from_warehouse": from_code, "to_warehouse": to_code,
        }),
    )
    .await
    .map_err(|e| e.with_field("transit_warehouse_id"))
}

/// A transfer was shipped: every line is on its way, nothing arrived yet.
pub(crate) async fn start(
    conn: &mut PgConnection,
    document_id: Uuid,
    lines: &[TransferLine],
) -> Result<()> {
    let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
    let qtys: Vec<Decimal> = lines.iter().map(|l| l.qty).collect();
    sqlx::query(
        "INSERT INTO stock_transfer_progress (line_id, document_id, qty_shipped)
         SELECT r.line_id, $1, r.qty FROM unnest($2::uuid[], $3::numeric[]) AS r(line_id, qty)",
    )
    .bind(document_id)
    .bind(&ids)
    .bind(&qtys)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct LineWithProgress {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    qty: Decimal,
    qty_received: Option<Decimal>,
}

/// The lines of a transfer as a reader sees them: with what has arrived of
/// each, once it was shipped.
pub(crate) async fn lines_with_progress(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<Vec<Value>> {
    let lines: Vec<LineWithProgress> = sqlx::query_as(
        "SELECT l.id, l.line_no, l.item_id, l.qty, p.qty_received
         FROM stock_transfer_line l LEFT JOIN stock_transfer_progress p ON p.line_id = l.id
         WHERE l.document_id = $1 ORDER BY l.line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines
        .into_iter()
        .map(|l| serde_json::to_value(l).unwrap_or_default())
        .collect())
}

/// What is still on its way on each line of a shipped transfer, locked.
async fn left_on_lines(
    conn: &mut PgConnection,
    transfer_id: Uuid,
) -> Result<Vec<(Uuid, i32, Uuid, Decimal, Decimal)>> {
    Ok(sqlx::query_as(
        "SELECT l.id, l.line_no, l.item_id, p.qty_shipped, p.qty_received
         FROM stock_transfer_progress p JOIN stock_transfer_line l ON l.id = p.line_id
         WHERE p.document_id = $1 ORDER BY l.line_no
         FOR UPDATE OF p",
    )
    .bind(transfer_id)
    .fetch_all(&mut *conn)
    .await?)
}

// ---------------------------------------------------------- the receipt

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    transfer_id: Uuid,
    #[serde(default)]
    lines: Vec<ReceiptLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptLineInput {
    id: Option<Uuid>,
    transfer_line_id: Uuid,
    qty: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct ReceiptLine {
    id: Uuid,
    line_no: i32,
    transfer_line_id: Uuid,
    item_id: Uuid,
    qty: Decimal,
    value: Option<Decimal>,
}

async fn receipt_transfer(conn: &mut PgConnection, document_id: Uuid) -> Result<Option<Uuid>> {
    Ok(
        sqlx::query_scalar("SELECT transfer_id FROM stock_transfer_receipt WHERE document_id = $1")
            .bind(document_id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

async fn receipt_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<ReceiptLine>> {
    Ok(sqlx::query_as(
        "SELECT r.id, r.line_no, r.transfer_line_id, l.item_id, r.qty, r.value
         FROM stock_transfer_receipt_line r JOIN stock_transfer_line l ON l.id = r.transfer_line_id
         WHERE r.document_id = $1 ORDER BY r.line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

pub(crate) struct TransferReceipt;

#[async_trait]
impl DocumentHandler for TransferReceipt {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: RECEIPT.into(),
            module: String::new(),
            label: "Transfer receipt".into(),
            number_prefix: "TR".into(),
            fields: vec![
                FieldMeta::new(
                    "transfer_id",
                    "Transfer",
                    FieldType::document_in(STOCK_TRANSFER, &["in_transit"]),
                )
                .required(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new(
                        "transfer_line_id",
                        "Transfer line",
                        FieldType::line_of("transfer_id", STOCK_TRANSFER, "lines"),
                    )
                    .required(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).readonly(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .required(),
                    FieldMeta::new("value", "Value", FieldType::Decimal)
                        .money()
                        .readonly()
                        .help("Known once posted: the value that arrived."),
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
                FieldMeta::new(
                    "transfer_id",
                    "Transfer",
                    FieldType::document(STOCK_TRANSFER),
                )
                .readonly(),
                FieldMeta::new("total_value", "Value", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once posted: the value that arrived."),
            ],
            plural: "Transfer receipts".into(),
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: ReceiptBody =
            erpcraft_kernel::input::parse(body.clone(), "transfer receipt body")?;
        let transfer: Option<(Uuid, String)> =
            sqlx::query_as("SELECT company_id, doc_type FROM document WHERE id = $1")
                .bind(body.transfer_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        if !transfer.is_some_and(|(company, doc_type)| {
            company == doc.company_id && doc_type == STOCK_TRANSFER
        }) {
            return Err(Error::validation(
                "the transfer must be a stock transfer of the document's company",
            )
            .with_field("transfer_id"));
        }
        let current = receipt_transfer(&mut *ctx.conn, doc.id).await?;
        if current.is_some_and(|t| t != body.transfer_id) {
            return Err(Error::validation(
                "the transfer of a receipt cannot change; create a new receipt instead",
            )
            .with_field("transfer_id"));
        }
        let own: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM stock_transfer_line WHERE document_id = $1")
                .bind(body.transfer_id)
                .fetch_all(&mut *ctx.conn)
                .await?;
        for (index, line) in body.lines.iter().enumerate() {
            if !own.contains(&line.transfer_line_id) {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("transfer_line_id"),
                    "the line must be a line of the receipt's transfer",
                ));
            }
            if line.qty <= Decimal::ZERO {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("qty"),
                    "the quantity must be more than zero",
                ));
            }
        }
        if current.is_none() {
            sqlx::query(
                "INSERT INTO stock_transfer_receipt (document_id, transfer_id) VALUES ($1, $2)",
            )
            .bind(doc.id)
            .bind(body.transfer_id)
            .execute(&mut *ctx.conn)
            .await?;
            document::link(ctx, body.transfer_id, doc.id, "receipt").await?;
        }
        let ids = document::line_ids(
            &mut *ctx.conn,
            "stock_transfer_receipt_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM stock_transfer_receipt_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            let lines: Vec<Uuid> = body.lines.iter().map(|l| l.transfer_line_id).collect();
            let qtys: Vec<Decimal> = body.lines.iter().map(|l| l.qty).collect();
            sqlx::query(
                "INSERT INTO stock_transfer_receipt_line (id, document_id, line_no,
                     transfer_line_id, qty)
                 SELECT r.id, $1, r.line_no, r.transfer_line_id, r.qty
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[])
                    AS r (id, line_no, transfer_line_id, qty)",
            )
            .bind(doc.id)
            .bind(&ids)
            .bind(&line_nos)
            .bind(&lines)
            .bind(&qtys)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let moved = erpcraft_stock::value_moved(conn, doc.id).await?;
        Ok(json!({
            "transfer_id": body["transfer_id"],
            "total_value": moved.map(|(arrived, _)| arrived),
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        Ok(json!({
            "transfer_id": receipt_transfer(conn, doc.id).await?,
            "lines": receipt_lines(conn, doc.id).await?,
        }))
    }
}

// ---------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiveInput {
    /// Stock transfer id.
    id: Uuid,
    /// Date of the receipt. Today when omitted.
    doc_date: Option<NaiveDate>,
}

/// Drafts a receipt of everything still on its way on a transfer.
pub(crate) struct Receive;

#[async_trait]
impl Command for Receive {
    type Input = ReceiveInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_transfer.receive",
            "Draft a transfer receipt for a shipped transfer, with a line for everything still \
             on its way. Nothing is posted: change the quantities to what arrived and post the \
             receipt with stock_transfer_receipt.post.",
        )
        .permission("stock_transfer_receipt:create")
        .from_states(&["in_transit"])
        .creates(RECEIPT)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReceiveInput) -> Result<DocumentView> {
        let transfer = document::lock(ctx, STOCK_TRANSFER, input.id, "read").await?;
        if transfer.status != "in_transit" {
            return Err(Error::state(format!(
                "a {} transfer has nothing on its way to receive",
                transfer.status
            )));
        }
        let lines = left_on_lines(&mut *ctx.conn, transfer.id).await?;
        let rows: Vec<Value> = lines
            .iter()
            .filter(|(_, _, _, shipped, received)| shipped > received)
            .map(|(id, _, _, shipped, received)| {
                json!({ "transfer_line_id": id, "qty": shipped - received })
            })
            .collect();
        document::create(
            ctx,
            document::NewDocument {
                doc_type: RECEIPT.into(),
                company_id: transfer.company_id,
                doc_date: input
                    .doc_date
                    .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive()),
                memo: None,
                body: json!({ "transfer_id": transfer.id, "lines": rows }),
            },
        )
        .await
    }
}

pub(crate) struct PostReceipt;

#[async_trait]
impl Command for PostReceipt {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_transfer_receipt.post",
            "Post a draft transfer receipt: what arrived leaves the transfer's transit \
             warehouse at its average cost there and comes into the destination with exactly \
             that value; the general ledger moves it from the account for \
             `inventory_in_transit` to the destination's inventory account. No line may \
             receive more than is still on its way. The transfer is received once everything \
             has arrived. Posting a receipt that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("stock_transfer_receipt:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RECEIPT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} transfer receipt cannot be posted",
                doc.status
            )));
        }
        let transfer_id = receipt_transfer(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::validation("a transfer receipt needs its transfer"))?;
        // The transfer's lock makes two receipts of it wait for each other.
        let mut transfer = document::lock(ctx, STOCK_TRANSFER, transfer_id, "read").await?;
        if transfer.status != "in_transit" {
            return Err(Error::state(format!(
                "only a transfer in transit can be received; this one is {}",
                transfer.status
            )));
        }
        let (_, to, through) = load_transfer(&mut *ctx.conn, transfer.id).await?;
        let through =
            through.ok_or_else(|| Error::internal("a transfer in transit without its way"))?;
        let lines = receipt_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation(
                "a transfer receipt needs at least one line",
            ));
        }
        let mut left: Vec<(Uuid, i32, Uuid, Decimal, Decimal)> =
            left_on_lines(&mut *ctx.conn, transfer.id).await?;
        for line in &lines {
            let Some(on_way) = left.iter_mut().find(|l| l.0 == line.transfer_line_id) else {
                return Err(Error::internal("a receipt line without its transfer line"));
            };
            let open = on_way.3 - on_way.4;
            if line.qty > open {
                return Err(Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("qty"),
                    format_args!(
                        "{} of transfer line {} {} still on its way, not {}",
                        open.normalize(),
                        on_way.1,
                        if open == Decimal::ONE { "is" } else { "are" },
                        line.qty.normalize()
                    ),
                ));
            }
            on_way.4 += line.qty;
        }

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let pairs = lines
            .iter()
            .flat_map(|line| [(line.item_id, through), (line.item_id, to)])
            .collect();
        erpcraft_stock::lock_levels(&mut *ctx.conn, doc.company_id, pairs).await?;
        let places = erpcraft_stock::locations(&mut *ctx.conn, [through, to]).await?;
        let mut gl = vec![];
        let mut values = vec![];
        for line in &lines {
            let value = move_stock(
                ctx,
                &doc,
                &posting,
                &places,
                line.id,
                line.item_id,
                line.qty,
                (through, to),
                &mut gl,
            )
            .await?;
            values.push(Some(value));
        }
        if !gl.is_empty() {
            erpcraft_gl::post(ctx, &posting, &gl).await?;
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "stock_transfer_receipt_line",
            &ids,
            &[("value", &values)],
        )
        .await?;
        progress(&mut *ctx.conn, &lines, Decimal::ONE).await?;
        if left
            .iter()
            .all(|(_, _, _, shipped, received)| received >= shipped)
        {
            document::transition(ctx, &mut transfer, "received").await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

/// Count what the lines received (`sign` one) or take it back (minus one).
async fn progress(conn: &mut PgConnection, lines: &[ReceiptLine], sign: Decimal) -> Result<()> {
    let ids: Vec<Uuid> = lines.iter().map(|l| l.transfer_line_id).collect();
    let qtys: Vec<Decimal> = lines.iter().map(|l| l.qty * sign).collect();
    sqlx::query(
        "UPDATE stock_transfer_progress p SET qty_received = p.qty_received + r.qty
         FROM (SELECT line_id, sum(qty) AS qty
               FROM unnest($1::uuid[], $2::numeric[]) AS u(line_id, qty) GROUP BY line_id) r
         WHERE p.line_id = r.line_id",
    )
    .bind(&ids)
    .bind(&qtys)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(crate) struct ReverseReceipt;

#[async_trait]
impl Command for ReverseReceipt {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_transfer_receipt.reverse",
            "Cancel a posted transfer receipt by a second posting that takes the goods back from \
             the destination into the transit warehouse at the value they came for; they are on \
             their way again and the transfer is in transit. Refused if the goods have since \
             left the destination.",
        )
        .permission("stock_transfer_receipt:reverse")
        .from_states(&["posted"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RECEIPT, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} transfer receipt cannot be reversed",
                doc.status
            )));
        }
        let transfer_id = receipt_transfer(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::internal("a transfer receipt without its transfer"))?;
        let mut transfer = document::lock(ctx, STOCK_TRANSFER, transfer_id, "read").await?;
        left_on_lines(&mut *ctx.conn, transfer.id).await?;
        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        let lines = receipt_lines(&mut *ctx.conn, doc.id).await?;
        progress(&mut *ctx.conn, &lines, -Decimal::ONE).await?;
        if transfer.status == "received" {
            document::transition(ctx, &mut transfer, "in_transit").await?;
        }
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}

/// Reverses a transfer: one posted in one step, or one shipped of which
/// nothing has arrived.
pub(crate) struct ReverseTransfer;

#[async_trait]
impl Command for ReverseTransfer {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_transfer.reverse",
            "Cancel a posted stock transfer by a second posting that moves the stock back at \
             the value it came for. Refused if the stock has since left the destination. A \
             shipped transfer is taken back from the transit warehouse, only while none of it \
             has been received: reverse its receipts first.",
        )
        .permission("stock_transfer:reverse")
        .from_states(&["posted", "in_transit"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_TRANSFER, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" && doc.status != "in_transit" {
            return Err(Error::state(format!(
                "a {} stock transfer cannot be reversed{}",
                doc.status,
                if doc.status == "received" {
                    ": reverse its receipts first"
                } else {
                    ""
                }
            )));
        }
        if doc.status == "in_transit" {
            let lines = left_on_lines(&mut *ctx.conn, doc.id).await?;
            if lines
                .iter()
                .any(|(_, _, _, _, received)| *received > Decimal::ZERO)
            {
                return Err(Error::state(
                    "part of this transfer has been received: reverse its receipts first",
                ));
            }
        }
        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}

// ---------------------------------------------------------- in transit

/// Goods on their way to a warehouse: shipped on a transfer and not yet
/// received.
#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct InTransit {
    pub transfer_id: Uuid,
    pub transfer_line_id: Uuid,
    pub item_id: Uuid,
    /// Where the goods are going.
    pub warehouse_id: Uuid,
    pub qty: Decimal,
}

/// Everything a company has on its way, per transfer line, for planning:
/// what was shipped and has not arrived counts as coming in at the
/// destination.
pub async fn in_transit(conn: &mut PgConnection, company_id: Uuid) -> Result<Vec<InTransit>> {
    Ok(sqlx::query_as(
        "SELECT d.id AS transfer_id, l.id AS transfer_line_id, l.item_id,
                t.to_warehouse_id AS warehouse_id, p.qty_shipped - p.qty_received AS qty
         FROM stock_transfer_progress p
         JOIN stock_transfer_line l ON l.id = p.line_id
         JOIN stock_transfer t ON t.document_id = p.document_id
         JOIN document d ON d.id = p.document_id
         WHERE d.company_id = $1 AND d.status = 'in_transit'
           AND p.qty_shipped > p.qty_received
         ORDER BY d.doc_date, d.id, l.line_no",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InTransitInput {
    /// The company to report on.
    company_id: Uuid,
    /// Only goods going to this warehouse.
    to_warehouse_id: Option<Uuid>,
    /// Only this item.
    item_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct InTransitRow {
    transfer_id: Uuid,
    transfer_number: Option<String>,
    shipped_on: NaiveDate,
    item_id: Uuid,
    item_code: Option<String>,
    from_warehouse_id: Uuid,
    to_warehouse_id: Uuid,
    transit_warehouse_id: Uuid,
    qty_shipped: Decimal,
    qty_received: Decimal,
    /// Shipped and not received.
    qty_left: Decimal,
    /// What is left at the transit warehouse's average cost of the item:
    /// what it would arrive for now.
    value: Option<Decimal>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct InTransitOutput {
    company_id: Uuid,
    currency: String,
    total_value: Decimal,
    rows: Vec<InTransitRow>,
}

pub(crate) struct InTransitReport;

#[async_trait]
impl Command for InTransitReport {
    type Input = InTransitInput;
    type Output = InTransitOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "stock_transfer.in_transit",
            "Goods on their way between warehouses: every line of a shipped transfer not \
             received in full, with what was shipped, what has arrived and what is left, and \
             what is left is worth at the transit warehouse's average cost of the item. \
             Optionally for one destination warehouse or one item.",
        )
        .report("Goods in transit")
        .permission("stock_transfer:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: InTransitInput) -> Result<InTransitOutput> {
        ctx.require("stock_transfer:read", input.company_id)?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let decimals = company::currency_decimals(&mut *ctx.conn, input.company_id).await?;
        let items = ctx.actor.can_any("item:read");
        let rows: Vec<InTransitRow> = sqlx::query_as(
            "SELECT d.id AS transfer_id, d.number AS transfer_number, d.doc_date AS shipped_on,
                    l.item_id, CASE WHEN $5 THEN i.code END AS item_code,
                    t.from_warehouse_id, t.to_warehouse_id, t.transit_warehouse_id,
                    p.qty_shipped, p.qty_received, p.qty_shipped - p.qty_received AS qty_left,
                    CASE WHEN s.qty > 0
                         THEN round((p.qty_shipped - p.qty_received) * s.value / s.qty, $4)
                    END AS value
             FROM stock_transfer_progress p
             JOIN stock_transfer_line l ON l.id = p.line_id
             JOIN item i ON i.id = l.item_id
             JOIN stock_transfer t ON t.document_id = p.document_id
             JOIN document d ON d.id = p.document_id
             LEFT JOIN stock_level s
                 ON s.item_id = l.item_id AND s.warehouse_id = t.transit_warehouse_id
             WHERE d.company_id = $1 AND d.status = 'in_transit'
               AND p.qty_shipped > p.qty_received
               AND ($2::uuid IS NULL OR t.to_warehouse_id = $2)
               AND ($3::uuid IS NULL OR l.item_id = $3)
             ORDER BY d.doc_date, d.number, l.line_no",
        )
        .bind(input.company_id)
        .bind(input.to_warehouse_id)
        .bind(input.item_id)
        .bind(decimals as i32)
        .bind(items)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(InTransitOutput {
            company_id: input.company_id,
            currency,
            total_value: rows.iter().filter_map(|r| r.value).sum(),
            rows,
        })
    }
}
