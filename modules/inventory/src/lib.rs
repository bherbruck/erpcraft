//! Inventory module. Uses only the public module API of the kernel, the
//! stock ledger and the general ledger, as a module outside this
//! repository would.
//!
//! A stock adjustment is the first document that writes to two ledgers in
//! one posting: quantities and their value to the stock ledger, and the
//! same value to the general ledger. Both succeed or neither does.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, PgConnection, Result, StateMeta, async_trait,
    command, company, schemars,
};
use erpcraft_stock::{Cost, StockLine};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const STOCK_ADJUSTMENT: &str = "stock_adjustment";
const STOCK_TRANSFER: &str = "stock_transfer";

pub struct InventoryModule;

impl Module for InventoryModule {
    fn name(&self) -> &'static str {
        "inventory"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "inventory",
                sql: include_str!("../migrations/0001_inventory.sql"),
            },
            Migration {
                version: 2,
                name: "transfer",
                sql: include_str!("../migrations/0002_transfer.sql"),
            },
            Migration {
                version: 3,
                name: "inventory_account_rule",
                sql: include_str!("../migrations/0003_inventory_account_rule.sql"),
            },
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(StockAdjustment), Arc::new(StockTransfer)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(PostAdjustment),
            command(Reverse {
                doc_type: STOCK_ADJUSTMENT,
                label: "stock adjustment",
                name: "stock_adjustment.reverse",
                permission: "stock_adjustment:reverse",
                description: "Cancel a posted stock adjustment by a second posting that takes back exactly \
                              what the first recorded, in both ledgers. Refused if stock it added has since \
                              been used.",
            }),
            command(PostTransfer),
            command(Reverse {
                doc_type: STOCK_TRANSFER,
                label: "stock transfer",
                name: "stock_transfer.reverse",
                permission: "stock_transfer:reverse",
                description: "Cancel a posted stock transfer by a second posting that moves the stock back at \
                              the value it came for. Refused if the stock has since left the destination.",
            }),
        ]
    }
}

// ------------------------------------------------------------ document type

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    offset_account_id: Option<Uuid>,
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    item_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    unit_cost: Option<Decimal>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    unit_cost: Option<Decimal>,
}

#[derive(Debug, Default, sqlx::FromRow)]
struct Header {
    offset_account_id: Option<Uuid>,
}

async fn load_header(conn: &mut PgConnection, document_id: Uuid) -> Result<Header> {
    let header =
        sqlx::query_as("SELECT offset_account_id FROM stock_adjustment WHERE document_id = $1")
            .bind(document_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(header.unwrap_or_default())
}

async fn load_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Line>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, item_id, warehouse_id, qty, unit_cost
         FROM stock_adjustment_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

struct StockAdjustment;

#[async_trait]
impl DocumentHandler for StockAdjustment {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: STOCK_ADJUSTMENT.into(),
            module: String::new(),
            label: "Stock adjustment".into(),
            number_prefix: "SA".into(),
            fields: vec![FieldMeta::new(
                "offset_account_id",
                "Offset account",
                FieldType::link("account"),
            )],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    FieldMeta::new("warehouse_id", "Warehouse", FieldType::link("warehouse"))
                        .required(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .required(),
                    FieldMeta::new("unit_cost", "Unit cost", FieldType::Decimal).unit_price(),
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
                FieldMeta::new("total_value", "Value", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once posted: the value added to stock less the value removed."),
            ],
            ..DocTypeMeta::default()
        }
    }

    /// A draft may lack its offset account; it is required when it is posted.
    /// Whether there is enough stock is also decided then, by the ledger.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "stock adjustment body")?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            let fail = |field: &str, msg: &str| Error::line("lines", n, Some(field), msg);
            if line.qty.is_zero() {
                return Err(fail("qty", "the quantity cannot be zero"));
            }
            match (line.qty.is_sign_positive(), line.unit_cost) {
                (true, None) => return Err(fail("unit_cost", "stock added needs a unit cost")),
                (true, Some(cost)) if cost.is_sign_negative() => {
                    return Err(fail("unit_cost", "the unit cost cannot be negative"));
                }
                (false, Some(_)) => {
                    return Err(fail(
                        "unit_cost",
                        "stock removed takes no unit cost: it leaves at the average cost on hand",
                    ));
                }
                _ => {}
            }
        }

        let warehouses: Vec<Uuid> = body.lines.iter().map(|l| l.warehouse_id).collect();
        if !company::owns_all(&mut *ctx.conn, "warehouse", &warehouses, doc.company_id).await? {
            return Err(Error::validation(
                "every line needs a warehouse of the document's company",
            ));
        }
        let accounts: Vec<Uuid> = body.offset_account_id.into_iter().collect();
        if !company::owns_all(&mut *ctx.conn, "account", &accounts, doc.company_id).await? {
            return Err(Error::validation(
                "the offset account must belong to the document's company",
            ));
        }

        sqlx::query(
            "INSERT INTO stock_adjustment (document_id, offset_account_id)
             VALUES ($1, $2)
             ON CONFLICT (document_id) DO UPDATE SET
                offset_account_id = EXCLUDED.offset_account_id",
        )
        .bind(doc.id)
        .bind(body.offset_account_id)
        .execute(&mut *ctx.conn)
        .await?;
        let ids = document::line_ids(
            &mut *ctx.conn,
            "stock_adjustment_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM stock_adjustment_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO stock_adjustment_line (id, document_id, line_no, item_id, warehouse_id, qty, unit_cost)
                 SELECT r.id, $1, r.line_no, r.item_id, r.warehouse_id, r.qty, r.unit_cost
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::uuid[], $6::numeric[], $7::numeric[])
                    AS r (id, line_no, item_id, warehouse_id, qty, unit_cost)",
            )
            .bind(doc.id)
            .bind(&ids)
            .bind(&line_nos)
            .bind(body.lines.iter().map(|line| line.item_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.warehouse_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.unit_cost).collect::<Vec<_>>())
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The change in stock value, once the ledger has said what the
    /// stock removed was worth.
    async fn summary(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let moved = erpcraft_stock::value_moved(conn, doc.id).await?;
        Ok(json!({ "total_value": moved.map(|(added, removed)| added - removed) }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header = load_header(conn, doc.id).await?;
        Ok(json!({
            "offset_account_id": header.offset_account_id,
            "lines": load_lines(conn, doc.id).await?,
        }))
    }
}

// ----------------------------------------------------------------- commands

/// The account that carries the value of `item_id` in `warehouse_id`.
/// The account for the difference between what goods arriving on stock
/// below zero cost and what the missing goods left stock for. Asked only
/// when there is such a difference.
async fn variance_account(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    item_id: Uuid,
    warehouse_id: Uuid,
) -> Result<Uuid> {
    erpcraft_gl::account_for(
        &mut *ctx.conn,
        doc.company_id,
        "stock_variance",
        &[("item", item_id), ("warehouse", warehouse_id)],
    )
    .await
}

/// A ledger line for a signed amount: a debit when positive, a credit when
/// negative, nothing when zero.
fn signed(account_id: Uuid, amount: Decimal, source_line_id: Option<Uuid>) -> Option<GlLine> {
    (!amount.is_zero()).then(|| GlLine {
        account_id,
        debit: amount.max(Decimal::ZERO),
        credit: (-amount).max(Decimal::ZERO),
        currency: None,
        fx_rate: None,
        memo: None,
        source_line_id,
    })
}

async fn inventory_account(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    item_id: Uuid,
    warehouse_id: Uuid,
) -> Result<Uuid> {
    erpcraft_gl::account_for(
        &mut *ctx.conn,
        doc.company_id,
        "inventory",
        &[("item", item_id), ("warehouse", warehouse_id)],
    )
    .await
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PostInput {
    /// Stock adjustment id.
    id: Uuid,
}

struct PostAdjustment;

#[async_trait]
impl Command for PostAdjustment {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_adjustment.post",
            "Post a draft stock adjustment: the quantities go to the stock ledger and their \
             value to the general ledger, together or not at all, between the inventory \
             account that the account rules give each item and warehouse and the \
             adjustment's offset account. Stock added comes in at the \
             unit cost given; stock removed leaves at the average cost on hand. Posting an \
             adjustment that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("stock_adjustment:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_ADJUSTMENT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} stock adjustment cannot be posted",
                doc.status
            )));
        }

        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let Some(offset) = header.offset_account_id else {
            return Err(Error::validation(
                "a stock adjustment needs an offset account before it can be posted",
            ));
        };
        let lines = load_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation(
                "a stock adjustment needs at least one line",
            ));
        }
        let movements: Vec<StockLine> = lines
            .iter()
            .map(|line| StockLine {
                item_id: line.item_id,
                warehouse_id: line.warehouse_id,
                qty: line.qty,
                cost: line.unit_cost.map(Cost::Unit),
                source_line_id: Some(line.id),
            })
            .collect();

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(ctx, &posting, &movements, "adjustment").await?;

        // The value the stock ledger gave each movement, mirrored in the
        // general ledger line by line.
        // The offset account takes what the movement cost in all. For
        // goods that arrived on stock below zero, part of that is not stock
        // value but variance.
        let mut gl = vec![];
        for entry in &entries {
            if !entry.value.is_zero() {
                let inventory =
                    inventory_account(ctx, &doc, entry.item_id, entry.warehouse_id).await?;
                gl.extend(signed(inventory, entry.value, entry.source_line_id));
            }
            if !entry.variance.is_zero() {
                let variance =
                    variance_account(ctx, &doc, entry.item_id, entry.warehouse_id).await?;
                gl.extend(signed(variance, entry.variance, entry.source_line_id));
            }
            gl.extend(signed(offset, -entry.cost(), entry.source_line_id));
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }

        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReverseInput {
    /// Document id.
    id: Uuid,
    /// Accounting date of the reversal. Defaults to the date of the
    /// original posting.
    posting_date: Option<NaiveDate>,
    /// Memo of the reversing posting. Defaults to "Reversal of" and the
    /// document number.
    memo: Option<String>,
}

/// Reverses the one posting of a posted document of this module.
struct Reverse {
    doc_type: &'static str,
    label: &'static str,
    name: &'static str,
    permission: &'static str,
    description: &'static str,
}

#[async_trait]
impl Command for Reverse {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(self.name, self.description)
            .permission(self.permission)
            .from_states(&["posted"])
            .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, self.doc_type, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} {} cannot be reversed",
                doc.status, self.label
            )));
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

// ----------------------------------------------------------- stock transfer

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferBody {
    from_warehouse_id: Uuid,
    to_warehouse_id: Uuid,
    #[serde(default)]
    lines: Vec<TransferLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    item_id: Uuid,
    qty: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct TransferLine {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    qty: Decimal,
}

async fn load_transfer(conn: &mut PgConnection, document_id: Uuid) -> Result<(Uuid, Uuid)> {
    let warehouses = sqlx::query_as(
        "SELECT from_warehouse_id, to_warehouse_id FROM stock_transfer WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(warehouses)
}

async fn load_transfer_lines(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<Vec<TransferLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, item_id, qty FROM stock_transfer_line
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

struct StockTransfer;

#[async_trait]
impl DocumentHandler for StockTransfer {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: STOCK_TRANSFER.into(),
            module: String::new(),
            label: "Stock transfer".into(),
            number_prefix: "ST".into(),
            fields: vec![
                FieldMeta::new(
                    "from_warehouse_id",
                    "From warehouse",
                    FieldType::link("warehouse"),
                )
                .required(),
                FieldMeta::new(
                    "to_warehouse_id",
                    "To warehouse",
                    FieldType::link("warehouse"),
                )
                .required(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .required(),
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
                    "from_warehouse_id",
                    "From warehouse",
                    FieldType::link("warehouse"),
                )
                .readonly(),
                FieldMeta::new(
                    "to_warehouse_id",
                    "To warehouse",
                    FieldType::link("warehouse"),
                )
                .readonly(),
                FieldMeta::new("total_value", "Value", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once posted: the value that moved."),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: TransferBody =
            erpcraft_kernel::input::parse(body.clone(), "stock transfer body")?;
        if body.from_warehouse_id == body.to_warehouse_id {
            return Err(Error::validation(
                "a transfer needs two different warehouses",
            ));
        }
        for (index, line) in body.lines.iter().enumerate() {
            if line.qty <= Decimal::ZERO {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("qty"),
                    "the quantity must be more than zero",
                ));
            }
        }
        let warehouses = [body.from_warehouse_id, body.to_warehouse_id];
        if !company::owns_all(&mut *ctx.conn, "warehouse", &warehouses, doc.company_id).await? {
            return Err(Error::validation(
                "both warehouses must belong to the document's company",
            ));
        }

        sqlx::query(
            "INSERT INTO stock_transfer (document_id, from_warehouse_id, to_warehouse_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (document_id) DO UPDATE SET
                from_warehouse_id = EXCLUDED.from_warehouse_id,
                to_warehouse_id = EXCLUDED.to_warehouse_id",
        )
        .bind(doc.id)
        .bind(body.from_warehouse_id)
        .bind(body.to_warehouse_id)
        .execute(&mut *ctx.conn)
        .await?;
        let ids = document::line_ids(
            &mut *ctx.conn,
            "stock_transfer_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM stock_transfer_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO stock_transfer_line (id, document_id, line_no, item_id, qty)
                 SELECT r.id, $1, r.line_no, r.item_id, r.qty
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[])
                    AS r (id, line_no, item_id, qty)",
            )
            .bind(doc.id)
            .bind(&ids)
            .bind(&line_nos)
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.item_id)
                    .collect::<Vec<_>>(),
            )
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The two warehouses and, once posted, the value that moved.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let moved = erpcraft_stock::value_moved(conn, doc.id).await?;
        Ok(json!({
            "from_warehouse_id": body["from_warehouse_id"],
            "to_warehouse_id": body["to_warehouse_id"],
            "total_value": moved.map(|(arrived, _)| arrived),
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (from, to) = load_transfer(conn, doc.id).await?;
        Ok(json!({
            "from_warehouse_id": from,
            "to_warehouse_id": to,
            "lines": load_transfer_lines(conn, doc.id).await?,
        }))
    }
}

struct PostTransfer;

#[async_trait]
impl Command for PostTransfer {
    type Input = PostInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "stock_transfer.post",
            "Post a draft stock transfer: each item leaves the source warehouse at its average \
             cost there and arrives at the destination with exactly that value. The general \
             ledger hears of it only where the account rules give the item a different \
             inventory account in the two warehouses: then the value moves between them. \
             Posting a transfer that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("stock_transfer:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_TRANSFER, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} stock transfer cannot be posted",
                doc.status
            )));
        }
        let (from, to) = load_transfer(&mut *ctx.conn, doc.id).await?;
        let lines = load_transfer_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation(
                "a stock transfer needs at least one line",
            ));
        }

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;

        // Every level this transfer touches, locked up front in one order,
        // so that a transfer going the other way cannot deadlock with it.
        let pairs = lines
            .iter()
            .flat_map(|line| [(line.item_id, from), (line.item_id, to)])
            .collect();
        erpcraft_stock::lock_levels(&mut *ctx.conn, doc.company_id, pairs).await?;

        let mut gl = vec![];
        for line in &lines {
            let leaves = inventory_account(ctx, &doc, line.item_id, from).await?;
            let arrives = inventory_account(ctx, &doc, line.item_id, to).await?;
            let movement = |warehouse_id, qty, cost| StockLine {
                item_id: line.item_id,
                warehouse_id,
                qty,
                cost,
                source_line_id: Some(line.id),
            };
            let out = erpcraft_stock::post(
                ctx,
                &posting,
                &[movement(from, -line.qty, None)],
                "transfer",
            )
            .await?;
            // What left is what arrives, to the cent.
            let value = -out[0].value;
            let arrived = erpcraft_stock::post(
                ctx,
                &posting,
                &[movement(to, line.qty, Some(Cost::Total(value)))],
                "transfer",
            )
            .await?;
            // Not zero only where the destination stood below zero: the
            // part of what arrived that is not stock value there.
            let variance = arrived[0].variance;
            // The company's stock is worth what it was. Only where the two
            // warehouses keep the item on different accounts does the value
            // move in the general ledger.
            if leaves != arrives {
                gl.extend(signed(arrives, arrived[0].value, Some(line.id)));
                gl.extend(signed(leaves, -value, Some(line.id)));
            } else {
                gl.extend(signed(arrives, -variance, Some(line.id)));
            }
            if !variance.is_zero() {
                let account = variance_account(ctx, &doc, line.item_id, to).await?;
                gl.extend(signed(account, variance, Some(line.id)));
            }
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }

        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}
