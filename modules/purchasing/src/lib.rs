//! Purchasing module: order, receive, be invoiced.
//!
//! This is the first flow that crosses documents. A purchase order stays
//! open while receipts and invoices are posted against it, each one a
//! document of its own, linked to the order:
//!
//! - a **receipt** brings goods into stock at the order price and credits
//!   goods received not invoiced (GRNI);
//! - a **vendor invoice** clears GRNI by what the invoiced goods were
//!   received for, credits payables by what the vendor charges, and sends
//!   the difference to purchase price variance.
//!
//! Every receipt and invoice locks its order first. That lock is what
//! keeps two receipts from together receiving more than was ordered.
//!
//! Uses only the public module API of the kernel and the two ledgers.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, EntityMeta, Error,
    FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection, Result, StateMeta,
    async_trait, command, new_id, round_money, schemars,
};
use erpcraft_stock::{Cost, StockLine};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const PURCHASE_ORDER: &str = "purchase_order";
const PURCHASE_RECEIPT: &str = "purchase_receipt";
const VENDOR_INVOICE: &str = "vendor_invoice";

pub struct PurchasingModule;

impl Module for PurchasingModule {
    fn name(&self) -> &'static str {
        "purchasing"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "purchasing",
            sql: include_str!("../migrations/0001_purchasing.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        let account = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::link("account")).required()
        };
        vec![
            EntityMeta::new("vendor", "Vendor")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("purchasing_settings", "Purchasing settings")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(account("inventory_account_id", "Inventory account"))
                .field(account(
                    "grni_account_id",
                    "Goods received not invoiced account",
                ))
                .field(account("payable_account_id", "Payables account"))
                .field(account(
                    "price_variance_account_id",
                    "Purchase price variance account",
                ))
                .field(created())
                .company_field("company_id"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![
            Arc::new(PurchaseOrder),
            Arc::new(PurchaseReceipt),
            Arc::new(VendorInvoice),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(OrderStep {
                name: "purchase_order.approve",
                permission: "purchase_order:approve",
                description: "Approve a draft purchase order. It receives its number, can no longer be \
                              changed, and goods can be received against it.",
                action: "approve",
                to: "open",
            }),
            command(OrderStep {
                name: "purchase_order.cancel",
                permission: "purchase_order:cancel",
                description: "Cancel a purchase order against which nothing has been received.",
                action: "cancel",
                to: "cancelled",
            }),
            command(OrderStep {
                name: "purchase_order.close",
                permission: "purchase_order:close",
                description: "Close an open purchase order that will not be completed. An order closes \
                              by itself once everything ordered has been received and invoiced.",
                action: "close",
                to: "closed",
            }),
            command(PostReceipt),
            command(ReverseReceipt),
            command(PostInvoice),
            command(ReverseInvoice),
        ]
    }
}

// ------------------------------------------------------------------- shared

fn states(first: StateMeta) -> Vec<StateMeta> {
    vec![
        first,
        StateMeta::locked("posted", "Posted"),
        StateMeta::locked("reversed", "Reversed"),
    ]
}

fn post_and_reverse() -> Vec<(String, String)> {
    vec![
        ("draft".into(), "posted".into()),
        ("posted".into(), "reversed".into()),
    ]
}

#[derive(Debug, sqlx::FromRow)]
struct Settings {
    inventory_account_id: Uuid,
    grni_account_id: Uuid,
    payable_account_id: Uuid,
    price_variance_account_id: Uuid,
}

async fn settings(conn: &mut PgConnection, company_id: Uuid) -> Result<Settings> {
    sqlx::query_as(
        "SELECT inventory_account_id, grni_account_id, payable_account_id, price_variance_account_id
         FROM purchasing_settings WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| {
        Error::validation(
            "this company has no purchasing settings yet: they name the accounts to post to",
        )
    })
}

async fn currency_decimals(conn: &mut PgConnection, company_id: Uuid) -> Result<u32> {
    let decimals: i16 = sqlx::query_scalar(
        "SELECT cur.decimals FROM company c
         JOIN currency cur ON cur.code = c.base_currency WHERE c.id = $1",
    )
    .bind(company_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(decimals as u32)
}

/// One line of an order with how far it has got.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct OrderLine {
    id: Uuid,
    line_no: i32,
    item_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    unit_price: Decimal,
    qty_received: Decimal,
    #[serde(skip)]
    received_value: Decimal,
    qty_invoiced: Decimal,
    #[serde(skip)]
    cleared_value: Decimal,
}

impl OrderLine {
    fn complete(&self) -> bool {
        self.qty_received == self.qty && self.qty_invoiced == self.qty
    }
}

async fn order_lines(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<OrderLine>> {
    let lines = sqlx::query_as(
        "SELECT l.id, l.line_no, l.item_id, l.warehouse_id, l.qty, l.unit_price,
                COALESCE(p.qty_received, 0) AS qty_received,
                COALESCE(p.received_value, 0) AS received_value,
                COALESCE(p.qty_invoiced, 0) AS qty_invoiced,
                COALESCE(p.cleared_value, 0) AS cleared_value
         FROM purchase_order_line l
         LEFT JOIN purchase_order_progress p ON p.line_id = l.id
         WHERE l.document_id = $1 ORDER BY l.line_no",
    )
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

async fn save_progress(conn: &mut PgConnection, line: &OrderLine) -> Result<()> {
    sqlx::query(
        "INSERT INTO purchase_order_progress
            (line_id, qty_received, received_value, qty_invoiced, cleared_value)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (line_id) DO UPDATE SET
            qty_received = EXCLUDED.qty_received,
            received_value = EXCLUDED.received_value,
            qty_invoiced = EXCLUDED.qty_invoiced,
            cleared_value = EXCLUDED.cleared_value",
    )
    .bind(line.id)
    .bind(line.qty_received)
    .bind(line.received_value)
    .bind(line.qty_invoiced)
    .bind(line.cleared_value)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The order a receipt or an invoice belongs to, locked until the command
/// ends, with its lines by id. The caller must be allowed to read the order.
async fn lock_order(
    ctx: &mut Ctx<'_>,
    order_id: Uuid,
) -> Result<(Document, BTreeMap<Uuid, OrderLine>)> {
    let order = document::lock(ctx, PURCHASE_ORDER, order_id, "read").await?;
    let lines = order_lines(&mut *ctx.conn, order.id).await?;
    Ok((order, lines.into_iter().map(|l| (l.id, l)).collect()))
}

/// Save the progress of an order's lines, then close the order if it is
/// now complete, or reopen it if it no longer is.
async fn settle_order(
    ctx: &mut Ctx<'_>,
    order: &mut Document,
    lines: &BTreeMap<Uuid, OrderLine>,
) -> Result<()> {
    for line in lines.values() {
        save_progress(&mut *ctx.conn, line).await?;
    }
    // An order closes by itself once it is complete. A closed order never
    // holds goods received and not invoiced, so it reopens when a reversal
    // leaves it with some. An order someone closed short stays closed
    // otherwise.
    let complete = lines.values().all(OrderLine::complete);
    let unbilled = lines.values().any(|l| l.qty_invoiced != l.qty_received);
    match order.status.as_str() {
        "open" if complete => document::transition(ctx, order, "closed").await,
        "closed" if unbilled => document::transition(ctx, order, "open").await,
        _ => Ok(()),
    }
}

/// The purchase order a draft receipt or invoice names: it must be an order
/// of the same company.
async fn check_order(ctx: &mut Ctx<'_>, doc: &Document, order_id: Uuid) -> Result<()> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM document WHERE id = $1 AND doc_type = $2 AND company_id = $3",
    )
    .bind(order_id)
    .bind(PURCHASE_ORDER)
    .bind(doc.company_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    match status.as_deref() {
        None => Err(Error::validation(
            "purchase_order_id must be a purchase order of the document's company",
        )),
        // An approved order's lines can no longer change, so documents
        // may refer to them.
        Some("open") => Ok(()),
        Some(status) => Err(Error::conflict(format!(
            "the purchase order is {status}; documents can be created only against an open order"
        ))),
    }
}

/// Every id in `line_ids` must be a line of the order, and none twice.
async fn check_order_lines(ctx: &mut Ctx<'_>, order_id: Uuid, line_ids: &[Uuid]) -> Result<()> {
    let mut ids = line_ids.to_vec();
    ids.sort();
    ids.dedup();
    if ids.len() != line_ids.len() {
        return Err(Error::validation(
            "an order line can appear only once in a document",
        ));
    }
    let known: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM purchase_order_line WHERE id = ANY($1) AND document_id = $2",
    )
    .bind(&ids)
    .bind(order_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    if known != ids.len() as i64 {
        return Err(Error::validation(
            "every line must refer to a line of the purchase order",
        ));
    }
    Ok(())
}

/// The original posting of a posted document.
async fn original_posting(
    conn: &mut PgConnection,
    doc: &Document,
) -> Result<erpcraft_kernel::Posting> {
    document::postings_for(conn, doc.id)
        .await?
        .into_iter()
        .find(|p| p.reverses_id.is_none())
        .ok_or_else(|| Error::internal("a posted document has no posting"))
}

fn gl_line(account_id: Uuid, debit: Decimal, credit: Decimal, source: Uuid) -> GlLine {
    GlLine {
        account_id,
        debit,
        credit,
        currency: None,
        fx_rate: None,
        memo: None,
        source_line_id: Some(source),
    }
}

fn trim(qty: Decimal) -> Decimal {
    qty.normalize()
}

// ----------------------------------------------------------- purchase order

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderBody {
    vendor_id: Uuid,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderLineInput {
    item_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    unit_price: Decimal,
}

struct PurchaseOrder;

#[async_trait]
impl DocumentHandler for PurchaseOrder {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: PURCHASE_ORDER.into(),
            module: String::new(),
            label: "Purchase order".into(),
            number_prefix: "PO".into(),
            fields: vec![
                FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor")).required(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    FieldMeta::new("warehouse_id", "Deliver to", FieldType::link("warehouse"))
                        .required(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal).required(),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal).required(),
                    FieldMeta::new("qty_received", "Received", FieldType::Decimal).readonly(),
                    FieldMeta::new("qty_invoiced", "Invoiced", FieldType::Decimal).readonly(),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("open", "Open"),
                StateMeta::locked("closed", "Closed"),
                StateMeta::locked("cancelled", "Cancelled"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "open".into()),
                ("draft".into(), "cancelled".into()),
                ("open".into(), "cancelled".into()),
                ("open".into(), "closed".into()),
                // Reversing the receipt or invoice that completed it.
                ("closed".into(), "open".into()),
            ],
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: OrderBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid purchase order body: {e}")))?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.qty <= Decimal::ZERO {
                return Err(Error::validation(format!(
                    "line {n}: the quantity must be more than zero"
                )));
            }
            if line.unit_price.is_sign_negative() {
                return Err(Error::validation(format!(
                    "line {n}: the unit price cannot be negative"
                )));
            }
            if line.unit_price.normalize().scale() > 6 {
                return Err(Error::validation(format!(
                    "line {n}: a unit price has at most 6 decimal places"
                )));
            }
        }
        let mut warehouses: Vec<Uuid> = body.lines.iter().map(|l| l.warehouse_id).collect();
        warehouses.sort();
        warehouses.dedup();
        let known: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM warehouse WHERE id = ANY($1) AND company_id = $2",
        )
        .bind(&warehouses)
        .bind(doc.company_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        if known != warehouses.len() as i64 {
            return Err(Error::validation(
                "every line needs a warehouse of the document's company",
            ));
        }

        sqlx::query(
            "INSERT INTO purchase_order (document_id, vendor_id) VALUES ($1, $2)
             ON CONFLICT (document_id) DO UPDATE SET vendor_id = EXCLUDED.vendor_id",
        )
        .bind(doc.id)
        .bind(body.vendor_id)
        .execute(&mut *ctx.conn)
        .await?;
        sqlx::query("DELETE FROM purchase_order_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO purchase_order_line (id, document_id, line_no, item_id, warehouse_id, qty, unit_price)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.item_id)
            .bind(line.warehouse_id)
            .bind(line.qty)
            .bind(line.unit_price)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let vendor: Uuid =
            sqlx::query_scalar("SELECT vendor_id FROM purchase_order WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(json!({ "vendor_id": vendor, "lines": order_lines(conn, doc.id).await? }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocInput {
    /// Document id.
    id: Uuid,
}

/// Moves a purchase order to another state of its workflow.
struct OrderStep {
    name: &'static str,
    permission: &'static str,
    description: &'static str,
    action: &'static str,
    to: &'static str,
}

#[async_trait]
impl Command for OrderStep {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(self.name, self.description).permission(self.permission)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut order = document::lock(ctx, PURCHASE_ORDER, input.id, self.action).await?;
        if order.status == self.to {
            ctx.no_change();
            return document::view(ctx, order).await;
        }
        let allowed: &[&str] = match self.to {
            "open" => &["draft"],
            "cancelled" => &["draft", "open"],
            _ => &["open"],
        };
        if !allowed.contains(&order.status.as_str()) {
            return Err(Error::conflict(format!(
                "a {} purchase order cannot be {}",
                order.status,
                match self.to {
                    "open" => "approved",
                    other => other,
                }
            )));
        }
        let lines = order_lines(&mut *ctx.conn, order.id).await?;
        match self.to {
            "open" => {
                if lines.is_empty() {
                    return Err(Error::validation(
                        "a purchase order needs at least one line",
                    ));
                }
                let active: bool = sqlx::query_scalar(
                    "SELECT v.active FROM purchase_order o JOIN vendor v ON v.id = o.vendor_id
                     WHERE o.document_id = $1",
                )
                .bind(order.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
                if !active {
                    return Err(Error::validation("the vendor is inactive"));
                }
                // Fail now rather than at the first receipt.
                settings(&mut *ctx.conn, order.company_id).await?;
                document::assign_number(ctx, &mut order).await?;
            }
            "cancelled" => {
                if lines.iter().any(|l| !l.qty_received.is_zero()) {
                    return Err(Error::conflict(
                        "goods have been received against this order; close it instead of cancelling it",
                    ));
                }
            }
            _ => {
                if lines.iter().any(|l| l.qty_invoiced != l.qty_received) {
                    return Err(Error::conflict(
                        "goods have been received that are not yet invoiced; the order must stay open for the invoice",
                    ));
                }
            }
        }
        document::transition(ctx, &mut order, self.to).await?;
        document::view(ctx, order).await
    }
}

// --------------------------------------------------------- purchase receipt

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    purchase_order_id: Uuid,
    #[serde(default)]
    lines: Vec<ReceiptLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptLineInput {
    order_line_id: Uuid,
    qty: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct ReceiptLine {
    id: Uuid,
    line_no: i32,
    order_line_id: Uuid,
    qty: Decimal,
    value: Option<Decimal>,
}

async fn receipt_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<ReceiptLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, qty, value FROM purchase_receipt_line
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

/// Store the header row of a receipt or an invoice. The order it belongs
/// to is fixed once the document exists.
async fn save_order_ref(
    ctx: &mut Ctx<'_>,
    table: &str,
    doc: &Document,
    order_id: Uuid,
    kind: &str,
) -> Result<()> {
    let existing: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT purchase_order_id FROM {table} WHERE document_id = $1"
    ))
    .bind(doc.id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    match existing {
        Some(current) if current != order_id => Err(Error::validation(
            "the purchase order of a document cannot change; create a new document instead",
        )),
        Some(_) => Ok(()),
        None => {
            check_order(ctx, doc, order_id).await?;
            sqlx::query(&format!(
                "INSERT INTO {table} (document_id, purchase_order_id) VALUES ($1, $2)"
            ))
            .bind(doc.id)
            .bind(order_id)
            .execute(&mut *ctx.conn)
            .await?;
            document::link(ctx, order_id, doc.id, kind).await
        }
    }
}

struct PurchaseReceipt;

#[async_trait]
impl DocumentHandler for PurchaseReceipt {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: PURCHASE_RECEIPT.into(),
            module: String::new(),
            label: "Purchase receipt".into(),
            number_prefix: "GR".into(),
            fields: vec![
                FieldMeta::new("purchase_order_id", "Purchase order", FieldType::Uuid)
                    .required()
                    .immutable(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("order_line_id", "Order line", FieldType::Uuid).required(),
                    FieldMeta::new("qty", "Quantity received", FieldType::Decimal).required(),
                    FieldMeta::new("value", "Value", FieldType::Decimal).readonly(),
                ],
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: ReceiptBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid purchase receipt body: {e}")))?;
        for (index, line) in body.lines.iter().enumerate() {
            if line.qty <= Decimal::ZERO {
                return Err(Error::validation(format!(
                    "line {}: the quantity must be more than zero",
                    index + 1
                )));
            }
        }
        save_order_ref(
            ctx,
            "purchase_receipt",
            doc,
            body.purchase_order_id,
            "receipt",
        )
        .await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.purchase_order_id, &ids).await?;

        sqlx::query("DELETE FROM purchase_receipt_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO purchase_receipt_line (id, document_id, line_no, order_line_id, qty)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.order_line_id)
            .bind(line.qty)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let order: Uuid = sqlx::query_scalar(
            "SELECT purchase_order_id FROM purchase_receipt WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        Ok(json!({ "purchase_order_id": order, "lines": receipt_lines(conn, doc.id).await? }))
    }
}

struct PostReceipt;

#[async_trait]
impl Command for PostReceipt {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "purchase_receipt.post",
            "Post a draft receipt against an open purchase order: the goods come into stock at \
             the order price, and their value is debited to inventory and credited to goods \
             received not invoiced. An order can be received in several parts, never more than \
             was ordered. Posting a receipt that is already posted returns it unchanged.",
        )
        .permission("purchase_receipt:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PURCHASE_RECEIPT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::conflict(format!(
                "a {} receipt cannot be posted",
                doc.status
            )));
        }
        let order_id: Uuid = sqlx::query_scalar(
            "SELECT purchase_order_id FROM purchase_receipt WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        if order.status != "open" {
            return Err(Error::conflict(format!(
                "the purchase order is {}; goods can be received only against an open order",
                order.status
            )));
        }
        let lines = receipt_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a receipt needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;

        let mut movements = vec![];
        for line in &lines {
            let ordered = order_lines
                .get(&line.order_line_id)
                .ok_or_else(|| Error::internal("receipt line without its order line"))?;
            let open = ordered.qty - ordered.qty_received;
            if line.qty > open {
                return Err(Error::validation(format!(
                    "line {}: {} are still to be received on order line {}, not {}",
                    line.line_no,
                    trim(open),
                    ordered.line_no,
                    trim(line.qty)
                )));
            }
            movements.push(StockLine {
                item_id: ordered.item_id,
                warehouse_id: ordered.warehouse_id,
                qty: line.qty,
                cost: Some(Cost::Unit(ordered.unit_price)),
                source_line_id: Some(line.id),
            });
        }

        document::assign_number(ctx, &mut doc).await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(&mut *ctx.conn, &posting, &movements).await?;

        let mut gl = vec![];
        for (line, entry) in lines.iter().zip(&entries) {
            sqlx::query("UPDATE purchase_receipt_line SET value = $2 WHERE id = $1")
                .bind(line.id)
                .bind(entry.value)
                .execute(&mut *ctx.conn)
                .await?;
            if let Some(ordered) = order_lines.get_mut(&line.order_line_id) {
                ordered.qty_received += line.qty;
                ordered.received_value += entry.value;
            }
            if !entry.value.is_zero() {
                gl.push(gl_line(
                    accounts.inventory_account_id,
                    entry.value,
                    Decimal::ZERO,
                    line.id,
                ));
                gl.push(gl_line(
                    accounts.grni_account_id,
                    Decimal::ZERO,
                    entry.value,
                    line.id,
                ));
            }
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }

        settle_order(ctx, &mut order, &order_lines).await?;
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
    /// original posting, whose period must then still be open.
    posting_date: Option<NaiveDate>,
    memo: Option<String>,
}

struct ReverseReceipt;

#[async_trait]
impl Command for ReverseReceipt {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "purchase_receipt.reverse",
            "Cancel a posted receipt: the goods leave stock again at the value they came for, \
             and the order line is open to be received again. Refused if the goods have been \
             invoiced, or have since left the warehouse.",
        )
        .permission("purchase_receipt:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PURCHASE_RECEIPT, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::conflict(format!(
                "a {} receipt cannot be reversed",
                doc.status
            )));
        }
        let order_id: Uuid = sqlx::query_scalar(
            "SELECT purchase_order_id FROM purchase_receipt WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;

        for line in receipt_lines(&mut *ctx.conn, doc.id).await? {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("receipt line without its order line"))?;
            if ordered.qty_received - line.qty < ordered.qty_invoiced {
                return Err(Error::conflict(format!(
                    "line {}: these goods have been invoiced; reverse the invoice first",
                    line.line_no
                )));
            }
            ordered.qty_received -= line.qty;
            ordered.received_value -= line.value.unwrap_or_default();
            // An invoice clears the average value of everything received on
            // its line, so after a partial invoice the value left may not
            // be this receipt's own, to the cent.
            let left = ordered.received_value - ordered.cleared_value;
            let unbilled = ordered.qty_received - ordered.qty_invoiced;
            if left.is_sign_negative() || (unbilled.is_zero() && !left.is_zero()) {
                return Err(Error::conflict(format!(
                    "line {}: part of this order line has been invoiced at the average value of its receipts; reverse that invoice first",
                    line.line_no
                )));
            }
        }

        let original = original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        settle_order(ctx, &mut order, &order_lines).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}

// ----------------------------------------------------------- vendor invoice

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceBody {
    purchase_order_id: Uuid,
    vendor_reference: Option<String>,
    #[serde(default)]
    lines: Vec<InvoiceLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceLineInput {
    order_line_id: Uuid,
    qty: Decimal,
    unit_price: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct InvoiceLine {
    id: Uuid,
    line_no: i32,
    order_line_id: Uuid,
    qty: Decimal,
    unit_price: Decimal,
    cleared_value: Option<Decimal>,
}

async fn invoice_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<InvoiceLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, qty, unit_price, cleared_value
         FROM vendor_invoice_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

struct VendorInvoice;

#[async_trait]
impl DocumentHandler for VendorInvoice {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: VENDOR_INVOICE.into(),
            module: String::new(),
            label: "Vendor invoice".into(),
            number_prefix: "VI".into(),
            fields: vec![
                FieldMeta::new("purchase_order_id", "Purchase order", FieldType::Uuid)
                    .required()
                    .immutable(),
                FieldMeta::new(
                    "vendor_reference",
                    "Vendor's invoice number",
                    FieldType::Text,
                ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("order_line_id", "Order line", FieldType::Uuid).required(),
                    FieldMeta::new("qty", "Quantity invoiced", FieldType::Decimal).required(),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal).required(),
                ],
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: InvoiceBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid vendor invoice body: {e}")))?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.qty <= Decimal::ZERO {
                return Err(Error::validation(format!(
                    "line {n}: the quantity must be more than zero"
                )));
            }
            if line.unit_price.is_sign_negative() {
                return Err(Error::validation(format!(
                    "line {n}: the unit price cannot be negative"
                )));
            }
            if line.unit_price.normalize().scale() > 6 {
                return Err(Error::validation(format!(
                    "line {n}: a unit price has at most 6 decimal places"
                )));
            }
        }
        save_order_ref(
            ctx,
            "vendor_invoice",
            doc,
            body.purchase_order_id,
            "invoice",
        )
        .await?;
        let reference = body
            .vendor_reference
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        sqlx::query("UPDATE vendor_invoice SET vendor_reference = $2 WHERE document_id = $1")
            .bind(doc.id)
            .bind(reference)
            .execute(&mut *ctx.conn)
            .await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.purchase_order_id, &ids).await?;

        sqlx::query("DELETE FROM vendor_invoice_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO vendor_invoice_line (id, document_id, line_no, order_line_id, qty, unit_price)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.order_line_id)
            .bind(line.qty)
            .bind(line.unit_price)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (order, reference): (Uuid, Option<String>) = sqlx::query_as(
            "SELECT purchase_order_id, vendor_reference FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        Ok(json!({
            "purchase_order_id": order,
            "vendor_reference": reference,
            "lines": invoice_lines(conn, doc.id).await?,
        }))
    }
}

struct PostInvoice;

#[async_trait]
impl Command for PostInvoice {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "vendor_invoice.post",
            "Post a draft vendor invoice against a purchase order. Only goods already received \
             can be invoiced. Payables are credited with what the vendor charges, goods received \
             not invoiced is debited with what those goods were received for, and any difference \
             goes to purchase price variance. Posting an invoice that is already posted returns \
             it unchanged.",
        )
        .permission("vendor_invoice:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, VENDOR_INVOICE, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::conflict(format!(
                "a {} invoice cannot be posted",
                doc.status
            )));
        }
        let order_id: Uuid = sqlx::query_scalar(
            "SELECT purchase_order_id FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        if order.status != "open" {
            return Err(Error::conflict(format!(
                "the purchase order is {}; only an open order can be invoiced",
                order.status
            )));
        }
        let lines = invoice_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("an invoice needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        let decimals = currency_decimals(&mut *ctx.conn, doc.company_id).await?;

        let mut gl = vec![];
        for line in &lines {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("invoice line without its order line"))?;
            let open_qty = ordered.qty_received - ordered.qty_invoiced;
            if line.qty > open_qty {
                return Err(Error::validation(format!(
                    "line {}: {} have been received and not yet invoiced on order line {}, not {}",
                    line.line_no,
                    trim(open_qty),
                    ordered.line_no,
                    trim(line.qty)
                )));
            }
            // What these goods were received for. The last of them takes
            // the last of the value, so nothing is left in GRNI.
            let open_value = ordered.received_value - ordered.cleared_value;
            let cleared = if line.qty == open_qty {
                open_value
            } else {
                round_money(line.qty * open_value / open_qty, decimals).min(open_value)
            };
            let charged = round_money(line.qty * line.unit_price, decimals);
            ordered.qty_invoiced += line.qty;
            ordered.cleared_value += cleared;
            sqlx::query("UPDATE vendor_invoice_line SET cleared_value = $2 WHERE id = $1")
                .bind(line.id)
                .bind(cleared)
                .execute(&mut *ctx.conn)
                .await?;

            if !cleared.is_zero() {
                gl.push(gl_line(
                    accounts.grni_account_id,
                    cleared,
                    Decimal::ZERO,
                    line.id,
                ));
            }
            if !charged.is_zero() {
                gl.push(gl_line(
                    accounts.payable_account_id,
                    Decimal::ZERO,
                    charged,
                    line.id,
                ));
            }
            // The vendor charged more than the goods were received for: a
            // cost. Less: a gain.
            let variance = charged - cleared;
            if variance > Decimal::ZERO {
                gl.push(gl_line(
                    accounts.price_variance_account_id,
                    variance,
                    Decimal::ZERO,
                    line.id,
                ));
            } else if variance < Decimal::ZERO {
                gl.push(gl_line(
                    accounts.price_variance_account_id,
                    Decimal::ZERO,
                    -variance,
                    line.id,
                ));
            }
        }

        document::assign_number(ctx, &mut doc).await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }
        settle_order(ctx, &mut order, &order_lines).await?;
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

struct ReverseInvoice;

#[async_trait]
impl Command for ReverseInvoice {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "vendor_invoice.reverse",
            "Cancel a posted vendor invoice. The goods it covered count as received and not \
             invoiced again, and an order it had completed is reopened.",
        )
        .permission("vendor_invoice:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, VENDOR_INVOICE, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::conflict(format!(
                "a {} invoice cannot be reversed",
                doc.status
            )));
        }
        let order_id: Uuid = sqlx::query_scalar(
            "SELECT purchase_order_id FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        for line in invoice_lines(&mut *ctx.conn, doc.id).await? {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("invoice line without its order line"))?;
            ordered.qty_invoiced -= line.qty;
            ordered.cleared_value -= line.cleared_value.unwrap_or_default();
        }

        let original = original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        settle_order(ctx, &mut order, &order_lines).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
