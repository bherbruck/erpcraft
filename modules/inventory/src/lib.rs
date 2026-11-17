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
    command, new_id, schemars,
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
    inventory_account_id: Option<Uuid>,
    offset_account_id: Option<Uuid>,
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
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
    inventory_account_id: Option<Uuid>,
    offset_account_id: Option<Uuid>,
}

async fn load_header(conn: &mut PgConnection, document_id: Uuid) -> Result<Header> {
    let header = sqlx::query_as(
        "SELECT inventory_account_id, offset_account_id FROM stock_adjustment WHERE document_id = $1",
    )
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

/// How many of `ids` are rows of `table` belonging to `company_id`.
async fn owned(
    conn: &mut PgConnection,
    table: &str,
    ids: &mut Vec<Uuid>,
    company_id: Uuid,
) -> Result<bool> {
    ids.sort();
    ids.dedup();
    let known: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {table} WHERE id = ANY($1) AND company_id = $2"
    ))
    .bind(&*ids)
    .bind(company_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(known == ids.len() as i64)
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
            fields: vec![
                FieldMeta::new(
                    "inventory_account_id",
                    "Inventory account",
                    FieldType::link("account"),
                ),
                FieldMeta::new(
                    "offset_account_id",
                    "Offset account",
                    FieldType::link("account"),
                ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    FieldMeta::new("warehouse_id", "Warehouse", FieldType::link("warehouse"))
                        .required(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal).required(),
                    FieldMeta::new("unit_cost", "Unit cost", FieldType::Decimal),
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

    /// A draft may lack its accounts; they are required when it is posted.
    /// Whether there is enough stock is also decided then, by the ledger.
    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid stock adjustment body: {e}")))?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            let fail = |msg: &str| Error::validation(format!("line {n}: {msg}"));
            if line.qty.is_zero() {
                return Err(fail("the quantity cannot be zero"));
            }
            match (line.qty.is_sign_positive(), line.unit_cost) {
                (true, None) => return Err(fail("stock added needs a unit cost")),
                (true, Some(cost)) if cost.is_sign_negative() => {
                    return Err(fail("the unit cost cannot be negative"));
                }
                (false, Some(_)) => {
                    return Err(fail(
                        "stock removed takes no unit cost: it leaves at the average cost on hand",
                    ));
                }
                _ => {}
            }
        }

        let mut warehouses: Vec<Uuid> = body.lines.iter().map(|l| l.warehouse_id).collect();
        if !owned(&mut *ctx.conn, "warehouse", &mut warehouses, doc.company_id).await? {
            return Err(Error::validation(
                "every line needs a warehouse of the document's company",
            ));
        }
        let mut accounts: Vec<Uuid> = [body.inventory_account_id, body.offset_account_id]
            .into_iter()
            .flatten()
            .collect();
        if !owned(&mut *ctx.conn, "account", &mut accounts, doc.company_id).await? {
            return Err(Error::validation(
                "the accounts must belong to the document's company",
            ));
        }

        sqlx::query(
            "INSERT INTO stock_adjustment (document_id, inventory_account_id, offset_account_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (document_id) DO UPDATE SET
                inventory_account_id = EXCLUDED.inventory_account_id,
                offset_account_id = EXCLUDED.offset_account_id",
        )
        .bind(doc.id)
        .bind(body.inventory_account_id)
        .bind(body.offset_account_id)
        .execute(&mut *ctx.conn)
        .await?;
        sqlx::query("DELETE FROM stock_adjustment_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO stock_adjustment_line (id, document_id, line_no, item_id, warehouse_id, qty, unit_cost)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.item_id)
            .bind(line.warehouse_id)
            .bind(line.qty)
            .bind(line.unit_cost)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header = load_header(conn, doc.id).await?;
        Ok(json!({
            "inventory_account_id": header.inventory_account_id,
            "offset_account_id": header.offset_account_id,
            "lines": load_lines(conn, doc.id).await?,
        }))
    }
}

// ----------------------------------------------------------------- commands

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
             value to the general ledger, together or not at all. Stock added comes in at the \
             unit cost given; stock removed leaves at the average cost on hand. Posting an \
             adjustment that is already posted returns it unchanged.",
        )
        .permission("stock_adjustment:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_ADJUSTMENT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::conflict(format!(
                "a {} stock adjustment cannot be posted",
                doc.status
            )));
        }

        let header = load_header(&mut *ctx.conn, doc.id).await?;
        let (Some(inventory), Some(offset)) =
            (header.inventory_account_id, header.offset_account_id)
        else {
            return Err(Error::validation(
                "a stock adjustment needs an inventory account and an offset account before it can be posted",
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

        document::assign_number(ctx, &mut doc).await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(&mut *ctx.conn, &posting, &movements).await?;

        // The value the stock ledger gave each movement, mirrored in the
        // general ledger line by line.
        let mut gl = vec![];
        for entry in entries.iter().filter(|e| !e.value.is_zero()) {
            let amount = entry.value.abs();
            let (debit, credit) = if entry.value.is_sign_positive() {
                (inventory, offset)
            } else {
                (offset, inventory)
            };
            for (account_id, debit, credit) in [
                (debit, amount, Decimal::ZERO),
                (credit, Decimal::ZERO, amount),
            ] {
                gl.push(GlLine {
                    account_id,
                    debit,
                    credit,
                    currency: None,
                    fx_rate: None,
                    memo: None,
                    source_line_id: entry.source_line_id,
                });
            }
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
        CommandSpec::action(self.name, self.description).permission(self.permission)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, self.doc_type, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::conflict(format!(
                "a {} {} cannot be reversed",
                doc.status, self.label
            )));
        }
        let original = document::postings_for(&mut *ctx.conn, doc.id)
            .await?
            .into_iter()
            .find(|p| p.reverses_id.is_none())
            .ok_or_else(|| Error::internal("a posted document has no posting"))?;
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
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal).required(),
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

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: TransferBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid stock transfer body: {e}")))?;
        if body.from_warehouse_id == body.to_warehouse_id {
            return Err(Error::validation(
                "a transfer needs two different warehouses",
            ));
        }
        for (index, line) in body.lines.iter().enumerate() {
            if line.qty <= Decimal::ZERO {
                return Err(Error::validation(format!(
                    "line {}: the quantity must be more than zero",
                    index + 1
                )));
            }
        }
        let mut warehouses = vec![body.from_warehouse_id, body.to_warehouse_id];
        if !owned(&mut *ctx.conn, "warehouse", &mut warehouses, doc.company_id).await? {
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
        sqlx::query("DELETE FROM stock_transfer_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO stock_transfer_line (id, document_id, line_no, item_id, qty)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.item_id)
            .bind(line.qty)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
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
             cost there and arrives at the destination with exactly that value. Nothing is \
             posted to the general ledger, because the company's stock value does not change. \
             Posting a transfer that is already posted returns it unchanged.",
        )
        .permission("stock_transfer:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PostInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, STOCK_TRANSFER, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::conflict(format!(
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

        document::assign_number(ctx, &mut doc).await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;

        // Every level this transfer touches, locked up front in one order,
        // so that a transfer going the other way cannot deadlock with it.
        let pairs = lines
            .iter()
            .flat_map(|line| [(line.item_id, from), (line.item_id, to)])
            .collect();
        erpcraft_stock::lock_levels(&mut *ctx.conn, doc.company_id, pairs).await?;

        for line in &lines {
            let movement = |warehouse_id, qty, cost| StockLine {
                item_id: line.item_id,
                warehouse_id,
                qty,
                cost,
                source_line_id: Some(line.id),
            };
            let out =
                erpcraft_stock::post(&mut *ctx.conn, &posting, &[movement(from, -line.qty, None)])
                    .await?;
            // What left is what arrives, to the cent.
            let value = -out[0].value;
            erpcraft_stock::post(
                &mut *ctx.conn,
                &posting,
                &[movement(to, line.qty, Some(Cost::Total(value)))],
            )
            .await?;
        }

        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}
