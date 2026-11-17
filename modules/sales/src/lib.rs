//! Sales module: order, deliver, invoice.
//!
//! The mirror image of purchasing. A sales order stays open while
//! deliveries and invoices are posted against it, each one a document of
//! its own, linked to the order:
//!
//! - a **delivery** takes goods out of stock at their moving average cost,
//!   debits cost of goods sold and credits inventory with that value;
//! - a **sales invoice** debits receivables and credits revenue by what the
//!   customer is charged for goods already delivered.
//!
//! Every delivery and invoice locks its order first. That lock is what
//! keeps two deliveries from together delivering more than was ordered.
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
use erpcraft_stock::StockLine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const SALES_ORDER: &str = "sales_order";
const SALES_DELIVERY: &str = "sales_delivery";
const SALES_INVOICE: &str = "sales_invoice";

pub struct SalesModule;

impl Module for SalesModule {
    fn name(&self) -> &'static str {
        "sales"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "sales",
            sql: include_str!("../migrations/0001_sales.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        let account = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::link("account")).required()
        };
        vec![
            EntityMeta::new("customer", "Customer")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("sales_settings", "Sales settings")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(account("receivable_account_id", "Receivables account"))
                .field(account("revenue_account_id", "Revenue account"))
                .field(account("cogs_account_id", "Cost of goods sold account"))
                .field(account("inventory_account_id", "Inventory account"))
                .field(created())
                .company_field("company_id"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![
            Arc::new(SalesOrder),
            Arc::new(SalesDelivery),
            Arc::new(SalesInvoice),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(OrderStep {
                name: "sales_order.approve",
                permission: "sales_order:approve",
                description: "Approve a draft sales order. It receives its number, can no longer be \
                              changed, and goods can be delivered against it.",
                action: "approve",
                to: "open",
            }),
            command(OrderStep {
                name: "sales_order.cancel",
                permission: "sales_order:cancel",
                description: "Cancel a sales order against which nothing has been delivered.",
                action: "cancel",
                to: "cancelled",
            }),
            command(OrderStep {
                name: "sales_order.close",
                permission: "sales_order:close",
                description: "Close an open sales order that will not be completed. An order closes \
                              by itself once everything ordered has been delivered and invoiced.",
                action: "close",
                to: "closed",
            }),
            command(PostDelivery),
            command(ReverseDelivery),
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
    receivable_account_id: Uuid,
    revenue_account_id: Uuid,
    cogs_account_id: Uuid,
    inventory_account_id: Uuid,
}

async fn settings(conn: &mut PgConnection, company_id: Uuid) -> Result<Settings> {
    sqlx::query_as(
        "SELECT receivable_account_id, revenue_account_id, cogs_account_id, inventory_account_id
         FROM sales_settings WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| {
        Error::validation(
            "this company has no sales settings yet: they name the accounts to post to",
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
    qty_delivered: Decimal,
    qty_invoiced: Decimal,
}

impl OrderLine {
    fn complete(&self) -> bool {
        self.qty_delivered == self.qty && self.qty_invoiced == self.qty
    }
}

async fn order_lines(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<OrderLine>> {
    let lines = sqlx::query_as(
        "SELECT l.id, l.line_no, l.item_id, l.warehouse_id, l.qty, l.unit_price,
                COALESCE(p.qty_delivered, 0) AS qty_delivered,
                COALESCE(p.qty_invoiced, 0) AS qty_invoiced
         FROM sales_order_line l
         LEFT JOIN sales_order_progress p ON p.line_id = l.id
         WHERE l.document_id = $1 ORDER BY l.line_no",
    )
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

async fn save_progress(conn: &mut PgConnection, line: &OrderLine) -> Result<()> {
    sqlx::query(
        "INSERT INTO sales_order_progress (line_id, qty_delivered, qty_invoiced)
         VALUES ($1, $2, $3)
         ON CONFLICT (line_id) DO UPDATE SET
            qty_delivered = EXCLUDED.qty_delivered,
            qty_invoiced = EXCLUDED.qty_invoiced",
    )
    .bind(line.id)
    .bind(line.qty_delivered)
    .bind(line.qty_invoiced)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The order a delivery or an invoice belongs to, locked until the command
/// ends, with its lines by id. The caller must be allowed to read the order.
async fn lock_order(
    ctx: &mut Ctx<'_>,
    order_id: Uuid,
) -> Result<(Document, BTreeMap<Uuid, OrderLine>)> {
    let order = document::lock(ctx, SALES_ORDER, order_id, "read").await?;
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
    // holds goods delivered and not invoiced, so it reopens when a reversal
    // leaves it with some. An order someone closed short stays closed
    // otherwise.
    let complete = lines.values().all(OrderLine::complete);
    let unbilled = lines.values().any(|l| l.qty_invoiced != l.qty_delivered);
    match order.status.as_str() {
        "open" if complete => document::transition(ctx, order, "closed").await,
        "closed" if unbilled => document::transition(ctx, order, "open").await,
        _ => Ok(()),
    }
}

/// The sales order a draft delivery or invoice names: it must be an order
/// of the same company.
async fn check_order(ctx: &mut Ctx<'_>, doc: &Document, order_id: Uuid) -> Result<()> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM document WHERE id = $1 AND doc_type = $2 AND company_id = $3",
    )
    .bind(order_id)
    .bind(SALES_ORDER)
    .bind(doc.company_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    match status.as_deref() {
        None => Err(Error::validation(
            "sales_order_id must be a sales order of the document's company",
        )),
        // An approved order's lines can no longer change, so documents
        // may refer to them.
        Some("open") => Ok(()),
        Some(status) => Err(Error::conflict(format!(
            "the sales order is {status}; documents can be created only against an open order"
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
        "SELECT count(*) FROM sales_order_line WHERE id = ANY($1) AND document_id = $2",
    )
    .bind(&ids)
    .bind(order_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    if known != ids.len() as i64 {
        return Err(Error::validation(
            "every line must refer to a line of the sales order",
        ));
    }
    Ok(())
}

/// Store the header row of a delivery or an invoice. The order it belongs
/// to is fixed once the document exists.
async fn save_order_ref(
    ctx: &mut Ctx<'_>,
    table: &str,
    doc: &Document,
    order_id: Uuid,
    kind: &str,
) -> Result<()> {
    let existing: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT sales_order_id FROM {table} WHERE document_id = $1"
    ))
    .bind(doc.id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    match existing {
        Some(current) if current != order_id => Err(Error::validation(
            "the sales order of a document cannot change; create a new document instead",
        )),
        Some(_) => Ok(()),
        None => {
            check_order(ctx, doc, order_id).await?;
            sqlx::query(&format!(
                "INSERT INTO {table} (document_id, sales_order_id) VALUES ($1, $2)"
            ))
            .bind(doc.id)
            .bind(order_id)
            .execute(&mut *ctx.conn)
            .await?;
            document::link(ctx, order_id, doc.id, kind).await
        }
    }
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

// -------------------------------------------------------------- sales order

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderBody {
    customer_id: Uuid,
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

struct SalesOrder;

#[async_trait]
impl DocumentHandler for SalesOrder {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: SALES_ORDER.into(),
            module: String::new(),
            label: "Sales order".into(),
            number_prefix: "SO".into(),
            fields: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).required(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("item_id", "Item", FieldType::link("item")).required(),
                    FieldMeta::new("warehouse_id", "Ship from", FieldType::link("warehouse"))
                        .required(),
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal).required(),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal).required(),
                    FieldMeta::new("qty_delivered", "Delivered", FieldType::Decimal).readonly(),
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
                // Reversing the delivery or invoice that completed it.
                ("closed".into(), "open".into()),
            ],
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: OrderBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid sales order body: {e}")))?;
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
            "INSERT INTO sales_order (document_id, customer_id) VALUES ($1, $2)
             ON CONFLICT (document_id) DO UPDATE SET customer_id = EXCLUDED.customer_id",
        )
        .bind(doc.id)
        .bind(body.customer_id)
        .execute(&mut *ctx.conn)
        .await?;
        sqlx::query("DELETE FROM sales_order_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO sales_order_line (id, document_id, line_no, item_id, warehouse_id, qty, unit_price)
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
        let customer: Uuid =
            sqlx::query_scalar("SELECT customer_id FROM sales_order WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(json!({ "customer_id": customer, "lines": order_lines(conn, doc.id).await? }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocInput {
    /// Document id.
    id: Uuid,
}

/// Moves a sales order to another state of its workflow.
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
        let mut order = document::lock(ctx, SALES_ORDER, input.id, self.action).await?;
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
                "a {} sales order cannot be {}",
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
                    return Err(Error::validation("a sales order needs at least one line"));
                }
                let active: bool = sqlx::query_scalar(
                    "SELECT c.active FROM sales_order o JOIN customer c ON c.id = o.customer_id
                     WHERE o.document_id = $1",
                )
                .bind(order.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
                if !active {
                    return Err(Error::validation("the customer is inactive"));
                }
                // Fail now rather than at the first delivery.
                settings(&mut *ctx.conn, order.company_id).await?;
                document::assign_number(ctx, &mut order).await?;
            }
            "cancelled" => {
                if lines.iter().any(|l| !l.qty_delivered.is_zero()) {
                    return Err(Error::conflict(
                        "goods have been delivered against this order; close it instead of cancelling it",
                    ));
                }
            }
            _ => {
                if lines.iter().any(|l| l.qty_invoiced != l.qty_delivered) {
                    return Err(Error::conflict(
                        "goods have been delivered that are not yet invoiced; the order must stay open for the invoice",
                    ));
                }
            }
        }
        document::transition(ctx, &mut order, self.to).await?;
        document::view(ctx, order).await
    }
}

// ----------------------------------------------------------- sales delivery

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryBody {
    sales_order_id: Uuid,
    #[serde(default)]
    lines: Vec<DeliveryLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryLineInput {
    order_line_id: Uuid,
    qty: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct DeliveryLine {
    id: Uuid,
    line_no: i32,
    order_line_id: Uuid,
    qty: Decimal,
    value: Option<Decimal>,
}

async fn delivery_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<DeliveryLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, qty, value FROM sales_delivery_line
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

struct SalesDelivery;

#[async_trait]
impl DocumentHandler for SalesDelivery {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: SALES_DELIVERY.into(),
            module: String::new(),
            label: "Sales delivery".into(),
            number_prefix: "DN".into(),
            fields: vec![
                FieldMeta::new("sales_order_id", "Sales order", FieldType::Uuid)
                    .required()
                    .immutable(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::new("order_line_id", "Order line", FieldType::Uuid).required(),
                    FieldMeta::new("qty", "Quantity delivered", FieldType::Decimal).required(),
                    FieldMeta::new("value", "Cost", FieldType::Decimal).readonly(),
                ],
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: DeliveryBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::validation(format!("invalid sales delivery body: {e}")))?;
        for (index, line) in body.lines.iter().enumerate() {
            if line.qty <= Decimal::ZERO {
                return Err(Error::validation(format!(
                    "line {}: the quantity must be more than zero",
                    index + 1
                )));
            }
        }
        save_order_ref(ctx, "sales_delivery", doc, body.sales_order_id, "delivery").await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.sales_order_id, &ids).await?;

        sqlx::query("DELETE FROM sales_delivery_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO sales_delivery_line (id, document_id, line_no, order_line_id, qty)
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
        let order: Uuid =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_delivery WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(json!({ "sales_order_id": order, "lines": delivery_lines(conn, doc.id).await? }))
    }
}

struct PostDelivery;

#[async_trait]
impl Command for PostDelivery {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_delivery.post",
            "Post a draft delivery against an open sales order: the goods leave stock at their \
             average cost, which is debited to cost of goods sold and credited to inventory. An \
             order can be delivered in several parts, never more than was ordered, and never \
             more than is on hand. Posting a delivery that is already posted returns it unchanged.",
        )
        .permission("sales_delivery:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_DELIVERY, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::conflict(format!(
                "a {} delivery cannot be posted",
                doc.status
            )));
        }
        let order_id: Uuid =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_delivery WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        if order.status != "open" {
            return Err(Error::conflict(format!(
                "the sales order is {}; goods can be delivered only against an open order",
                order.status
            )));
        }
        let lines = delivery_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a delivery needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;

        let mut movements = vec![];
        for line in &lines {
            let ordered = order_lines
                .get(&line.order_line_id)
                .ok_or_else(|| Error::internal("delivery line without its order line"))?;
            let open = ordered.qty - ordered.qty_delivered;
            if line.qty > open {
                return Err(Error::validation(format!(
                    "line {}: {} are still to be delivered on order line {}, not {}",
                    line.line_no,
                    trim(open),
                    ordered.line_no,
                    trim(line.qty)
                )));
            }
            // No cost: the stock ledger values what goes out.
            movements.push(StockLine {
                item_id: ordered.item_id,
                warehouse_id: ordered.warehouse_id,
                qty: -line.qty,
                cost: None,
                source_line_id: Some(line.id),
            });
        }

        document::assign_number(ctx, &mut doc).await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(&mut *ctx.conn, &posting, &movements).await?;

        let mut gl = vec![];
        for (line, entry) in lines.iter().zip(&entries) {
            // The value of an issue is negative; its cost is that, positive.
            let cost = -entry.value;
            sqlx::query("UPDATE sales_delivery_line SET value = $2 WHERE id = $1")
                .bind(line.id)
                .bind(cost)
                .execute(&mut *ctx.conn)
                .await?;
            if let Some(ordered) = order_lines.get_mut(&line.order_line_id) {
                ordered.qty_delivered += line.qty;
            }
            if !cost.is_zero() {
                gl.push(gl_line(
                    accounts.cogs_account_id,
                    cost,
                    Decimal::ZERO,
                    line.id,
                ));
                gl.push(gl_line(
                    accounts.inventory_account_id,
                    Decimal::ZERO,
                    cost,
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

struct ReverseDelivery;

#[async_trait]
impl Command for ReverseDelivery {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_delivery.reverse",
            "Cancel a posted delivery: the goods come back into stock at the value they left \
             for, and the order line is open to be delivered again. Refused if the goods have \
             been invoiced.",
        )
        .permission("sales_delivery:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_DELIVERY, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::conflict(format!(
                "a {} delivery cannot be reversed",
                doc.status
            )));
        }
        let order_id: Uuid =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_delivery WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;

        for line in delivery_lines(&mut *ctx.conn, doc.id).await? {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("delivery line without its order line"))?;
            if ordered.qty_delivered - line.qty < ordered.qty_invoiced {
                return Err(Error::conflict(format!(
                    "line {}: these goods have been invoiced; reverse the invoice first",
                    line.line_no
                )));
            }
            ordered.qty_delivered -= line.qty;
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

// ------------------------------------------------------------ sales invoice

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceBody {
    sales_order_id: Uuid,
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
}

async fn invoice_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<InvoiceLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, qty, unit_price
         FROM sales_invoice_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

struct SalesInvoice;

#[async_trait]
impl DocumentHandler for SalesInvoice {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: SALES_INVOICE.into(),
            module: String::new(),
            label: "Sales invoice".into(),
            number_prefix: "SI".into(),
            fields: vec![
                FieldMeta::new("sales_order_id", "Sales order", FieldType::Uuid)
                    .required()
                    .immutable(),
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
            .map_err(|e| Error::validation(format!("invalid sales invoice body: {e}")))?;
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
        save_order_ref(ctx, "sales_invoice", doc, body.sales_order_id, "invoice").await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.sales_order_id, &ids).await?;

        sqlx::query("DELETE FROM sales_invoice_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, line) in body.lines.iter().enumerate() {
            sqlx::query(
                "INSERT INTO sales_invoice_line (id, document_id, line_no, order_line_id, qty, unit_price)
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
        let order: Uuid =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(json!({ "sales_order_id": order, "lines": invoice_lines(conn, doc.id).await? }))
    }
}

struct PostInvoice;

#[async_trait]
impl Command for PostInvoice {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_invoice.post",
            "Post a draft sales invoice against a sales order. Only goods already delivered \
             and not yet invoiced can be invoiced. Receivables are debited and revenue is \
             credited with what the customer is charged. Posting an invoice that is already \
             posted returns it unchanged.",
        )
        .permission("sales_invoice:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_INVOICE, input.id, "post").await?;
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
        let order_id: Uuid =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        if order.status != "open" {
            return Err(Error::conflict(format!(
                "the sales order is {}; only an open order can be invoiced",
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
            let open_qty = ordered.qty_delivered - ordered.qty_invoiced;
            if line.qty > open_qty {
                return Err(Error::validation(format!(
                    "line {}: {} have been delivered and not yet invoiced on order line {}, not {}",
                    line.line_no,
                    trim(open_qty),
                    ordered.line_no,
                    trim(line.qty)
                )));
            }
            ordered.qty_invoiced += line.qty;

            let charged = round_money(line.qty * line.unit_price, decimals);
            if !charged.is_zero() {
                gl.push(gl_line(
                    accounts.receivable_account_id,
                    charged,
                    Decimal::ZERO,
                    line.id,
                ));
                gl.push(gl_line(
                    accounts.revenue_account_id,
                    Decimal::ZERO,
                    charged,
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
            "sales_invoice.reverse",
            "Cancel a posted sales invoice. The goods it covered count as delivered and not \
             invoiced again, and an order it had completed is reopened.",
        )
        .permission("sales_invoice:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_INVOICE, input.id, "reverse").await?;
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
        let order_id: Uuid =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        for line in invoice_lines(&mut *ctx.conn, doc.id).await? {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("invoice line without its order line"))?;
            ordered.qty_invoiced -= line.qty;
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
