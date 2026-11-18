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
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, DynCommand,
    EntityMeta, Error, FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection, Result,
    StateMeta, async_trait, command, company, round_money, schemars,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use erpcraft_stock::StockLine;
use erpcraft_tax::Direction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod policy;
mod returns;

const SALES_ORDER: &str = "sales_order";
const SALES_DELIVERY: &str = "sales_delivery";
const SALES_INVOICE: &str = "sales_invoice";

pub struct SalesModule;

impl Module for SalesModule {
    fn name(&self) -> &'static str {
        "sales"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock", "open_items", "tax"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "sales",
                sql: include_str!("../migrations/0001_sales.sql"),
            },
            Migration {
                version: 2,
                name: "tax",
                sql: include_str!("../migrations/0002_tax.sql"),
            },
            Migration {
                version: 3,
                name: "inventory_account_rule",
                sql: include_str!("../migrations/0003_inventory_account_rule.sql"),
            },
            Migration {
                version: 4,
                name: "over_delivery",
                sql: include_str!("../migrations/0004_over_delivery.sql"),
            },
            Migration {
                version: 5,
                name: "returns",
                sql: include_str!("../migrations/0005_returns.sql"),
            },
            Migration {
                version: 6,
                name: "currency",
                sql: include_str!("../migrations/0006_currency.sql"),
            },
            Migration {
                version: 7,
                name: "credit_cap",
                sql: include_str!("../migrations/0007_credit_cap.sql"),
            },
            Migration {
                version: 8,
                name: "payment_terms",
                sql: include_str!("../migrations/0008_payment_terms.sql"),
            },
            Migration {
                version: 9,
                name: "lines",
                sql: include_str!("../migrations/0009_lines.sql"),
            },
        ]
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
                .field(
                    FieldMeta::new(
                        "payment_terms_id",
                        "Payment terms",
                        FieldType::link("payment_terms"),
                    )
                    .help("The customer's usual terms: what its orders and invoices start with."),
                )
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
                .field(created())
                .company_field("company_id"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![
            Arc::new(SalesOrder),
            Arc::new(SalesDelivery),
            Arc::new(SalesInvoice),
            Arc::new(returns::SalesReturn),
            Arc::new(returns::SalesCreditNote),
        ]
    }

    fn policies(&self) -> Vec<erpcraft_kernel::PolicyPoint> {
        policy::points()
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
            command(Follow {
                name: "sales_order.deliver",
                permission: "sales_delivery:create",
                description: "Draft a delivery for an open sales order, with a line for every \
                              order line and the quantity still to be delivered on it. Nothing \
                              is posted: change the quantities to what goes out, then post the \
                              delivery with sales_delivery.post.",
                doc_type: SALES_DELIVERY,
            }),
            command(Follow {
                name: "sales_order.invoice",
                permission: "sales_invoice:create",
                description: "Draft an invoice for an open sales order, with a line for \
                              everything delivered and not yet invoiced, at the price of the \
                              order. Nothing is posted: set the prices and tax codes, then post \
                              it with sales_invoice.post.",
                doc_type: SALES_INVOICE,
            }),
            command(PostDelivery),
            command(ReverseDelivery),
            command(PostInvoice),
            command(ReverseInvoice),
            command(returns::DraftReturn),
            command(returns::PostReturn),
            command(returns::ReverseReturn),
            command(returns::DraftCreditNote),
            command(returns::PostCreditNote),
            command(returns::ReverseCreditNote),
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
}

async fn settings(conn: &mut PgConnection, company_id: Uuid) -> Result<Settings> {
    sqlx::query_as(
        "SELECT receivable_account_id, revenue_account_id, cogs_account_id
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

/// One line of an order with how far it has got. Delivered and invoiced
/// are net: what came back has left `qty_delivered`, and what was credited
/// for goods that came back has left `qty_invoiced`. The returns and
/// credits themselves are counted apart.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct OrderLine {
    id: Uuid,
    line_no: i32,
    /// Both absent for a line that is not stock.
    item_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    /// For a line that is not stock: a charge, or an account.
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    /// A line that is not stock counts as delivered in full: invoicing it
    /// is what completes it.
    qty_delivered: Decimal,
    qty_invoiced: Decimal,
    /// Come back from the customer.
    qty_returned: Decimal,
    /// Of what came back, credited to the customer.
    qty_credited: Decimal,
    /// The line's net amount, as an invoice for all of it would charge:
    /// shown when the order is read.
    #[sqlx(default)]
    net_amount: Option<Decimal>,
}

impl OrderLine {
    /// Everything ordered has gone out, and everything that went out has
    /// been invoiced. Where the company's policy let more go out than was
    /// ordered, the extra goods must be invoiced too before the line is
    /// complete.
    fn complete(&self) -> bool {
        self.qty_delivered >= self.qty && self.qty_invoiced == self.qty_delivered
    }

    /// The item and warehouse of a stock line; refused for a line that is
    /// not stock, which nothing is delivered or returned of.
    fn stock(&self, collection_line: i32) -> Result<(Uuid, Uuid)> {
        self.item_id.zip(self.warehouse_id).ok_or_else(|| {
            Error::line(
                "lines",
                collection_line as usize,
                Some("order_line_id"),
                format_args!(
                    "order line {} is not stock: nothing is delivered or returned of it",
                    self.line_no
                ),
            )
        })
    }
}

async fn order_lines(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<OrderLine>> {
    let lines = sqlx::query_as(
        "SELECT l.id, l.line_no, l.item_id, l.warehouse_id, l.charge_id, l.account_id,
                l.description, l.qty, l.unit_price, l.discount_percent, l.discount_amount,
                CASE WHEN l.item_id IS NULL THEN l.qty
                     ELSE COALESCE(p.qty_delivered - p.qty_returned, 0) END AS qty_delivered,
                COALESCE(p.qty_invoiced - p.qty_credited, 0) AS qty_invoiced,
                COALESCE(p.qty_returned, 0) AS qty_returned,
                COALESCE(p.qty_credited, 0) AS qty_credited
         FROM sales_order_line l
         LEFT JOIN sales_order_progress p ON p.line_id = l.id
         WHERE l.document_id = $1 ORDER BY l.line_no",
    )
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

/// Save how far the lines of an order have got, in one statement.
async fn save_progress(conn: &mut PgConnection, lines: &BTreeMap<Uuid, OrderLine>) -> Result<()> {
    let lines: Vec<&OrderLine> = lines.values().collect();
    let column = |f: fn(&OrderLine) -> Decimal| lines.iter().map(|l| f(l)).collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO sales_order_progress
            (line_id, qty_delivered, qty_invoiced, qty_returned, qty_credited)
         SELECT * FROM unnest($1::uuid[], $2::numeric[], $3::numeric[], $4::numeric[],
                              $5::numeric[])
         ON CONFLICT (line_id) DO UPDATE SET
            qty_delivered = EXCLUDED.qty_delivered,
            qty_invoiced = EXCLUDED.qty_invoiced,
            qty_returned = EXCLUDED.qty_returned,
            qty_credited = EXCLUDED.qty_credited",
    )
    .bind(lines.iter().map(|l| l.id).collect::<Vec<_>>())
    // Stored as delivered and invoiced in all, returns and credits apart.
    .bind(column(|l| l.qty_delivered + l.qty_returned))
    .bind(column(|l| l.qty_invoiced + l.qty_credited))
    .bind(column(|l| l.qty_returned))
    .bind(column(|l| l.qty_credited))
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
/// now complete, or, after a reversal (`reversal`), reopen it if it no
/// longer is.
async fn settle_order(
    ctx: &mut Ctx<'_>,
    order: &mut Document,
    lines: &BTreeMap<Uuid, OrderLine>,
    reversal: bool,
) -> Result<()> {
    save_progress(&mut *ctx.conn, lines).await?;
    // An order closes by itself once it is complete. It reopens when a
    // reversal leaves it with goods delivered and not invoiced, which a
    // closed order otherwise does not hold. An order someone closed short
    // stays closed otherwise, and so does a closed order the company's
    // policy let a delivery or an invoice post against
    // (`sales_order.closed`): posting never reopens an order.
    let complete = lines.values().all(OrderLine::complete);
    let unbilled = lines.values().any(|l| l.qty_invoiced != l.qty_delivered);
    match order.status.as_str() {
        "open" if complete => document::transition(ctx, order, "closed").await,
        "closed" if unbilled && reversal => document::transition(ctx, order, "open").await,
        _ => Ok(()),
    }
}

/// Whether a delivery or an invoice (`doc_type`) may refer to an order in
/// `status`: an open order, or a closed one where the company's policy
/// allows it. `refused` ends the sentence that says why not.
async fn check_open<'l>(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    status: &str,
    lines: impl IntoIterator<Item = &'l OrderLine>,
    doc_type: &str,
    refused: &str,
) -> Result<()> {
    match status {
        "open" => Ok(()),
        "closed" => {
            let lines: Vec<&OrderLine> = lines.into_iter().collect();
            let facts = policy::closed_order(
                doc_type,
                lines.iter().all(|l| l.complete()),
                lines
                    .iter()
                    .map(|l| (l.qty, l.qty_delivered, l.qty_invoiced)),
            );
            let ruling = ctx
                .policy(policy::CLOSED_ORDER, company_id)
                .await?
                .ruling(facts)?;
            match ruling.decision {
                Decision::Allowed => Ok(()),
                Decision::Refused(refusal) => {
                    Err(Error::state(refusal.message.unwrap_or_else(|| {
                        format!("the sales order is closed; {refused}")
                    })))
                }
            }
        }
        status => Err(Error::state(format!(
            "the sales order is {status}; {refused}"
        ))),
    }
}

/// Ask the company's policy whether this (`doc_type`) may be done for the
/// order's customer, active or not.
async fn check_customer(ctx: &mut Ctx<'_>, order: &Document, doc_type: &str) -> Result<()> {
    let (code, active): (String, bool) = sqlx::query_as(
        "SELECT c.code, c.active FROM sales_order o JOIN customer c ON c.id = o.customer_id
         WHERE o.document_id = $1",
    )
    .bind(order.id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    let facts = policy::inactive_customer(doc_type, &code, active);
    ctx.decide(policy::INACTIVE_CUSTOMER, order.company_id, facts)
        .await
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
    // An approved order's lines can no longer change, so documents may
    // refer to them.
    let Some(status) = status else {
        return Err(Error::validation(
            "sales_order_id must be a sales order of the document's company",
        ));
    };
    let lines = match status.as_str() {
        "closed" => order_lines(&mut *ctx.conn, order_id).await?,
        _ => vec![],
    };
    check_open(
        ctx,
        doc.company_id,
        &status,
        &lines,
        &doc.doc_type,
        "documents can be created only against an open order",
    )
    .await
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

/// The payment terms an order or invoice is on.
fn terms_field(help: &str) -> FieldMeta {
    FieldMeta::new(
        "payment_terms_id",
        "Payment terms",
        FieldType::link("payment_terms"),
    )
    .help(help)
}

/// The exchange rate an invoice in a foreign currency states or posted at.
fn rate_field() -> FieldMeta {
    FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
        .rate(10)
        .help(
            "For an order in another currency: accounting currency per unit. The rate on record \
             for the document's date when left out; once posted, the rate it was posted at.",
        )
}

/// The currency of the order an invoice or credit note (`table`) follows,
/// and the rate the document states or, once posted, posted at.
async fn currency_of(
    conn: &mut PgConnection,
    table: &str,
    doc: &Document,
) -> Result<(String, Option<Decimal>)> {
    let found: Option<(String, Option<Decimal>)> = sqlx::query_as(&format!(
        "SELECT COALESCE(o.currency, {own}c.base_currency), t.fx_rate FROM {table} t
         LEFT JOIN sales_order o ON o.document_id = t.sales_order_id
         JOIN company c ON c.id = $2
         WHERE t.document_id = $1",
        // A direct sales invoice is in a currency of its own.
        own = if table == "sales_invoice" {
            "t.currency, "
        } else {
            ""
        }
    ))
    .bind(doc.id)
    .bind(doc.company_id)
    .fetch_optional(&mut *conn)
    .await?;
    match found {
        Some(found) => Ok(found),
        None => Ok((company::base_currency(conn, doc.company_id).await?, None)),
    }
}

/// A ledger line in the document's currency at its rate, when that is not
/// the accounting currency; the line as it is otherwise.
fn in_currency(mut line: GlLine, conversion: Option<&erpcraft_tax::Conversion>) -> GlLine {
    if let Some(conversion) = conversion {
        line.currency = Some(conversion.currency.clone());
        line.fx_rate = Some(conversion.rate);
    }
    line
}

/// What an invoice or credit note comes to in the accounting currency, as
/// net, tax and gross: the same as in its own currency when that is the
/// accounting currency; for a foreign one, once posted, what the posting
/// put on the control account (gross) and the tax accounts (tax), to the
/// cent; unknown before, while its rate is not fixed.
pub(crate) async fn accounting_totals(
    conn: &mut PgConnection,
    doc: &Document,
    foreign: bool,
    net: Decimal,
    tax: Option<Decimal>,
) -> Result<Value> {
    let (net, tax, gross) = if !foreign {
        (Some(net), tax, tax.map(|tax| net + tax))
    } else if doc.status == "draft" {
        (None, None, None)
    } else {
        match erpcraft_openitems::posted_value(conn, doc.id).await? {
            Some(gross) => {
                let tax = erpcraft_tax::posted_value(conn, doc.id).await?;
                (Some(gross - tax), Some(tax), Some(gross))
            }
            None => (None, None, None),
        }
    };
    Ok(json!({
        "total_net_accounting": net,
        "total_tax_accounting": tax,
        "total_gross_accounting": gross,
    }))
}

/// The summary fields [`accounting_totals`] fills.
pub(crate) fn accounting_fields() -> Vec<FieldMeta> {
    [
        ("total_net_accounting", "Net in accounting currency"),
        ("total_tax_accounting", "Tax in accounting currency"),
        ("total_gross_accounting", "Total in accounting currency"),
    ]
    .into_iter()
    .map(|(name, label)| {
        FieldMeta::new(name, label, FieldType::Decimal)
            .money()
            .readonly()
            .help("At the document's rate, as posted; unknown for a draft in another currency.")
    })
    .collect()
}

/// Put the accounting totals into a summary.
pub(crate) fn add_totals(summary: &mut Value, totals: Value) {
    if let (Some(summary), Value::Object(totals)) = (summary.as_object_mut(), totals) {
        summary.extend(totals);
    }
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

/// The customer of an order, for the summary of the order and of the
/// documents that follow from it.
async fn party_of(conn: &mut PgConnection, order_id: Uuid) -> Result<Option<(Uuid, String)>> {
    let party = sqlx::query_as(
        "SELECT p.id, p.name FROM sales_order o JOIN customer p ON p.id = o.customer_id
         WHERE o.document_id = $1",
    )
    .bind(order_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(party)
}

/// The header of the summary of a document that follows from an order:
/// the order its body names, and the order's customer.
async fn follows(conn: &mut PgConnection, body: &Value) -> Result<Value> {
    let order = document::body::uuid(&body["sales_order_id"]);
    let party = match (order, document::body::uuid(&body["customer_id"])) {
        (Some(order), _) => party_of(conn, order).await?,
        (None, Some(customer)) => {
            sqlx::query_as("SELECT id, name FROM customer WHERE id = $1")
                .bind(customer)
                .fetch_optional(&mut *conn)
                .await?
        }
        (None, None) => None,
    };
    Ok(json!({
        "sales_order_id": order,
        "customer_id": party.as_ref().map(|(id, _)| id),
        "customer_name": party.as_ref().map(|(_, name)| name),
    }))
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
    /// The accounting currency when omitted.
    currency: Option<String>,
    /// The customer's terms when omitted.
    payment_terms_id: Option<Uuid>,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    /// A stock line: the item and the warehouse it leaves from.
    item_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    /// A line that is not stock: a charge or an account, and what it is.
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
}

/// The account a sales line that is not stock posts its revenue to: its
/// account, or its charge's (`charge_revenue` for the charge, else the
/// company's default for `charge_revenue`), else the revenue account.
pub(crate) async fn revenue_account(
    conn: &mut PgConnection,
    company_id: Uuid,
    revenue: Uuid,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
) -> Result<Uuid> {
    if let Some(account) = account_id {
        return Ok(account);
    }
    if let Some(charge) = charge_id
        && let Ok(account) =
            erpcraft_gl::account_for(conn, company_id, "charge_revenue", &[("charge", charge)])
                .await
    {
        return Ok(account);
    }
    Ok(revenue)
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
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code")).help(
                    "The currency of the prices, and of the invoices that follow. The company's \
                     accounting currency when left out; fixed once the order is approved.",
                ),
                terms_field(
                    "The customer's usual terms when left out; what its invoices start with.",
                ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item"))
                        .help("For a stock line, with the warehouse it leaves from."),
                    FieldMeta::new("warehouse_id", "Ship from", FieldType::link("warehouse")),
                ]
                .into_iter()
                .chain(erpcraft_tax::lines::line_kind_fields(
                    "For a line that is not stock and not a charge: the revenue account.",
                ))
                .chain([
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .required(),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .currency_of_document("currency")
                        .required(),
                ])
                .chain(erpcraft_tax::lines::discount_fields())
                .chain([
                    FieldMeta::new("net_amount", "Net", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                    FieldMeta::new("qty_delivered", "Delivered", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly()
                        .help("Net of what came back."),
                    FieldMeta::new("qty_invoiced", "Invoiced", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly()
                        .help("Net of what was credited for goods that came back."),
                    FieldMeta::new("qty_returned", "Returned", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly(),
                    FieldMeta::new("qty_credited", "Credited", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly()
                        .help("Of what came back, what the customer has been credited."),
                ])
                .collect(),
            }],
            alternatives: vec![erpcraft_tax::lines::line_kinds("lines")],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("open", "Open"),
                StateMeta::locked("closed", "Closed"),
                StateMeta::locked("cancelled", "Cancelled").unnumbered(),
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
            summary: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).readonly(),
                FieldMeta::new("customer_name", "Customer name", FieldType::Text)
                    .readonly()
                    .shown_from("customer_id", "name"),
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code")).readonly(),
                FieldMeta::new("total_net", "Total", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly()
                    .help("The lines at their prices, without tax, in the order's currency: an order carries none."),
            ]
            .into_iter()
            .chain(erpcraft_tax::lines::discount_and_charge_fields())
            .collect(),
            // 1: currency, accounting totals and their declarations.
            // 2: discounts and charges.
            summary_version: 2,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: OrderBody = erpcraft_kernel::input::parse(body.clone(), "sales order body")?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.item_id.is_some() != line.warehouse_id.is_some() {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("warehouse_id"),
                    "a stock line names both its item and its warehouse",
                ));
            }
            erpcraft_tax::lines::check_line(
                ctx,
                doc.company_id,
                n,
                line.item_id.is_some(),
                line.charge_id,
                line.account_id,
                line.qty,
                line.unit_price,
                line.discount_percent,
                line.discount_amount,
            )
            .await?;
        }
        let mut body = body;
        for line in &mut body.lines {
            line.discount_percent = line.discount_percent.filter(|p| !p.is_zero());
            line.discount_amount = line.discount_amount.filter(|a| !a.is_zero());
        }
        let warehouses: Vec<Uuid> = body.lines.iter().filter_map(|l| l.warehouse_id).collect();
        if !company::owns_all(&mut *ctx.conn, "warehouse", &warehouses, doc.company_id).await? {
            return Err(Error::validation(
                "every line needs a warehouse of the document's company",
            ));
        }

        let base = company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let currency = body.currency.filter(|c| *c != base);
        if let Some(currency) = &currency {
            erpcraft_gl::currency_decimals(&mut *ctx.conn, currency)
                .await
                .map_err(|e| e.with_field("currency"))?;
        }
        // The customer's terms unless the order names others.
        sqlx::query(
            "INSERT INTO sales_order (document_id, customer_id, currency, payment_terms_id)
             VALUES ($1, $2, $3,
                     COALESCE($4, (SELECT payment_terms_id FROM customer WHERE id = $2)))
             ON CONFLICT (document_id) DO UPDATE SET customer_id = EXCLUDED.customer_id,
                currency = EXCLUDED.currency, payment_terms_id = EXCLUDED.payment_terms_id",
        )
        .bind(doc.id)
        .bind(body.customer_id)
        .bind(currency)
        .bind(body.payment_terms_id)
        .execute(&mut *ctx.conn)
        .await?;
        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "sales_order_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_order_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO sales_order_line (id, document_id, line_no, item_id, warehouse_id, qty,
                    unit_price, charge_id, account_id, description, discount_percent,
                    discount_amount)
                 SELECT r.id, $1, r.line_no, r.item_id, r.warehouse_id, r.qty, r.unit_price,
                    r.charge_id, r.account_id, NULLIF(trim(r.description), ''),
                    r.discount_percent, r.discount_amount
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::uuid[], $6::numeric[],
                             $7::numeric[], $8::uuid[], $9::uuid[], $10::text[], $11::numeric[],
                             $12::numeric[])
                    AS r (id, line_no, item_id, warehouse_id, qty, unit_price, charge_id,
                          account_id, description, discount_percent, discount_amount)",
            )
            .bind(doc.id)
            .bind(&line_ids)
            .bind(&line_nos)
            .bind(body.lines.iter().map(|line| line.item_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.warehouse_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.unit_price).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.charge_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.account_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.description.clone()).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.discount_percent).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.discount_amount).collect::<Vec<_>>())
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The customer and what the order comes to: each line's quantity
    /// times its price, rounded as an invoice for it would be.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let party: Option<(Uuid, String)> =
            sqlx::query_as("SELECT id, name FROM customer WHERE id = $1")
                .bind(document::body::uuid(&body["customer_id"]))
                .fetch_optional(&mut *conn)
                .await?;
        let currency = match body["currency"].as_str() {
            Some(currency) => currency.to_owned(),
            None => company::base_currency(conn, doc.company_id).await?,
        };
        let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
        let total: Decimal = document::body::lines(body, "lines")
            .iter()
            .map(|l| erpcraft_tax::lines::body_line_net(l, decimals))
            .sum();
        let (discount, charges) = erpcraft_tax::lines::discount_and_charges(
            document::body::lines(body, "lines"),
            decimals,
            |line| !line["charge_id"].is_null(),
        );
        Ok(json!({
            "customer_id": party.as_ref().map(|(id, _)| id),
            "customer_name": party.as_ref().map(|(_, name)| name),
            "currency": currency,
            "total_net": total,
            "total_discount": discount,
            "total_charges": charges,
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (customer, currency, terms): (Uuid, String, Option<Uuid>) = sqlx::query_as(
            "SELECT o.customer_id, COALESCE(o.currency, c.base_currency), o.payment_terms_id
             FROM sales_order o
             JOIN company c ON c.id = $2 WHERE o.document_id = $1",
        )
        .bind(doc.id)
        .bind(doc.company_id)
        .fetch_one(&mut *conn)
        .await?;
        let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
        let mut lines = order_lines(conn, doc.id).await?;
        for line in &mut lines {
            line.net_amount = erpcraft_tax::line_net(
                line.qty,
                line.unit_price,
                line.discount_percent,
                line.discount_amount,
                decimals,
            );
        }
        Ok(json!({
            "customer_id": customer,
            "currency": currency,
            "payment_terms_id": terms,
            "lines": lines,
        }))
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
        let spec = CommandSpec::action(self.name, self.description).permission(self.permission);
        match self.to {
            "open" => spec.from_states(&["draft"]),
            "cancelled" => spec.from_states(&["draft", "open"]).destructive(),
            _ => spec.from_states(&["open"]),
        }
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
            return Err(Error::state(format!(
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
                check_customer(ctx, &order, SALES_ORDER).await?;
                // Fail now rather than at the first delivery.
                settings(&mut *ctx.conn, order.company_id).await?;
            }
            "cancelled" => {
                if lines
                    .iter()
                    .any(|l| !(l.qty_delivered + l.qty_returned).is_zero())
                {
                    return Err(Error::state(
                        "goods have been delivered against this order; close it instead of cancelling it",
                    ));
                }
            }
            _ => {
                if lines.iter().any(|l| l.qty_invoiced != l.qty_delivered) {
                    return Err(Error::state(
                        "goods have been delivered that are not yet invoiced; the order must stay open for the invoice",
                    ));
                }
            }
        }
        document::transition(ctx, &mut order, self.to).await?;
        document::view(ctx, order).await
    }
}

// ------------------------------------------------ what follows from an order

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FollowInput {
    /// Sales order id.
    id: Uuid,
    /// Date of the new document. Today when omitted.
    doc_date: Option<NaiveDate>,
}

/// Drafts the document that follows from an open sales order, with a line
/// for everything that is still to come. Nothing is posted: the draft is
/// there to be checked, changed and posted like one entered by hand.
struct Follow {
    name: &'static str,
    permission: &'static str,
    description: &'static str,
    /// The type of the document to draft.
    doc_type: &'static str,
}

#[async_trait]
impl Command for Follow {
    type Input = FollowInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(self.name, self.description)
            .permission(self.permission)
            .from_states(&["open"])
            .creates(self.doc_type)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: FollowInput) -> Result<DocumentView> {
        let order = document::lock(ctx, SALES_ORDER, input.id, "read").await?;
        let lines = order_lines(&mut *ctx.conn, order.id).await?;
        check_open(
            ctx,
            order.company_id,
            &order.status,
            &lines,
            self.doc_type,
            "a document can be drafted only from an open order",
        )
        .await?;
        let rows: Vec<Value> = if self.doc_type == SALES_DELIVERY {
            lines
                .iter()
                .filter(|l| l.qty_delivered < l.qty)
                .map(|l| json!({ "order_line_id": l.id, "qty": l.qty - l.qty_delivered }))
                .collect()
        } else {
            lines
                .iter()
                .filter(|l| l.qty_invoiced < l.qty_delivered)
                .map(|l| {
                    json!({
                        "order_line_id": l.id,
                        "qty": l.qty_delivered - l.qty_invoiced,
                        "unit_price": l.unit_price,
                    })
                })
                .collect()
        };
        if rows.is_empty() {
            return Err(Error::validation(if self.doc_type == SALES_DELIVERY {
                "everything on this order has been delivered"
            } else {
                "nothing has been delivered on this order that is not yet invoiced"
            }));
        }
        document::create(
            ctx,
            document::NewDocument {
                doc_type: self.doc_type.into(),
                company_id: order.company_id,
                doc_date: input
                    .doc_date
                    .unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive()),
                memo: None,
                body: json!({ "sales_order_id": order.id, "lines": rows }),
            },
        )
        .await
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
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
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
                FieldMeta::new(
                    "sales_order_id",
                    "Sales order",
                    FieldType::document_in(SALES_ORDER, &["open"]),
                )
                .required()
                .immutable(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new(
                        "order_line_id",
                        "Order line",
                        FieldType::line_of("sales_order_id", SALES_ORDER, "lines"),
                    )
                    .required(),
                    FieldMeta::new("qty", "Quantity delivered", FieldType::Decimal)
                        .quantity()
                        .required(),
                    FieldMeta::new("value", "Cost", FieldType::Decimal)
                        .money()
                        .readonly(),
                ],
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
            summary: vec![
                FieldMeta::new(
                    "sales_order_id",
                    "Sales order",
                    FieldType::document(SALES_ORDER),
                )
                .readonly(),
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).readonly(),
                FieldMeta::new("customer_name", "Customer name", FieldType::Text)
                    .readonly()
                    .shown_from("customer_id", "name"),
                FieldMeta::new("total_value", "Cost", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once posted: what the goods left stock for."),
            ],
            // 1: currency, accounting totals and their declarations.
            summary_version: 1,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: DeliveryBody =
            erpcraft_kernel::input::parse(body.clone(), "sales delivery body")?;
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
        save_order_ref(ctx, "sales_delivery", doc, body.sales_order_id, "delivery").await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.sales_order_id, &ids).await?;

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "sales_delivery_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_delivery_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO sales_delivery_line (id, document_id, line_no, order_line_id, qty)
                 SELECT r.id, $1, r.line_no, r.order_line_id, r.qty
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[])
                    AS r (id, line_no, order_line_id, qty)",
            )
            .bind(doc.id)
            .bind(&line_ids)
            .bind(&line_nos)
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.order_line_id)
                    .collect::<Vec<_>>(),
            )
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The order and its customer, and once posted the value of the goods:
    /// the sum of what the stock ledger valued each line at.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let mut summary = follows(conn, body).await?;
        let lines = document::body::lines(body, "lines");
        let total: Option<Decimal> = lines
            .iter()
            .map(|l| document::body::decimal(&l["value"]))
            .sum();
        summary["total_value"] = json!(total.filter(|_| !lines.is_empty()));
        Ok(summary)
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
             order can be delivered in several parts; more than was ordered only where the \
             company's policy allows it, and never more than is on hand. Posting a delivery \
             that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("sales_delivery:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_DELIVERY, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
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
        check_open(
            ctx,
            order.company_id,
            &order.status,
            order_lines.values(),
            SALES_DELIVERY,
            "goods can be delivered only against an open order",
        )
        .await?;
        check_customer(ctx, &order, SALES_DELIVERY).await?;
        let lines = delivery_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a delivery needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;

        let over_delivery = ctx.policy(policy::OVER_DELIVERY, doc.company_id).await?;
        let mut movements = vec![];
        // The inventory account of each line, by what is delivered from where.
        let mut inventory = vec![];
        for line in &lines {
            let ordered = order_lines
                .get(&line.order_line_id)
                .ok_or_else(|| Error::internal("delivery line without its order line"))?;
            let (item_id, warehouse_id) = ordered.stock(line.line_no)?;
            inventory.push(
                erpcraft_gl::account_for(
                    &mut *ctx.conn,
                    doc.company_id,
                    "inventory",
                    &[("item", item_id), ("warehouse", warehouse_id)],
                )
                .await?,
            );
            let facts = policy::over_delivery(ordered.qty, ordered.qty_delivered, line.qty);
            if let Decision::Refused(refusal) = over_delivery.evaluate(facts)? {
                let open = (ordered.qty - ordered.qty_delivered).max(Decimal::ZERO);
                let why = refusal.message.unwrap_or_else(|| {
                    format!(
                        "{} are still to be delivered on order line {}, not {}",
                        trim(open),
                        ordered.line_no,
                        trim(line.qty)
                    )
                });
                return Err(Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("qty"),
                    why,
                ));
            }
            // No cost: the stock ledger values what goes out.
            movements.push(StockLine {
                item_id,
                warehouse_id,
                qty: -line.qty,
                cost: None,
                source_line_id: Some(line.id),
            });
        }

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(ctx, &posting, &movements, "sales_delivery").await?;

        let mut gl = vec![];
        let mut costs = vec![];
        for ((line, entry), inventory) in lines.iter().zip(&entries).zip(inventory) {
            // The value of an issue is negative; its cost is that, positive.
            let cost = -entry.value;
            costs.push(Some(cost));
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
                gl.push(gl_line(inventory, Decimal::ZERO, cost, line.id));
            }
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "sales_delivery_line",
            &ids,
            &[("value", &costs)],
        )
        .await?;
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }

        settle_order(ctx, &mut order, &order_lines, false).await?;
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
    /// Memo of the reversing posting. Defaults to "Reversal of" and the
    /// document number.
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
        .from_states(&["posted"])
        .destructive()
        .permission("sales_delivery:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_DELIVERY, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
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
                return Err(Error::state(format!(
                    "line {}: these goods have been invoiced; reverse the invoice first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
            ordered.qty_delivered -= line.qty;
            // A return took goods back at the average cost of the line's
            // deliveries, not at this one's. Taking this delivery back may
            // not leave the line with a cost that goods no longer out on it
            // would have to carry, or with less than none.
            let left = returns::cost_out(&mut *ctx.conn, ordered.id).await?
                - line.value.unwrap_or_default();
            if left.is_sign_negative() && !left.is_zero()
                || (ordered.qty_delivered.is_zero() && !left.is_zero())
            {
                return Err(Error::state(format!(
                    "line {}: goods of this order line came back at the average cost of its deliveries; reverse that return first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
        }

        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        settle_order(ctx, &mut order, &order_lines, true).await?;
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}

// ------------------------------------------------------------ sales invoice

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceBody {
    /// The order invoiced; absent for a direct invoice (services), which
    /// names its customer and has lines of its own only.
    sales_order_id: Option<Uuid>,
    /// For a direct invoice: the customer.
    customer_id: Option<Uuid>,
    /// For a direct invoice: its currency, the accounting currency when
    /// omitted. An invoice of an order is in the order's.
    currency: Option<String>,
    /// For an order in a foreign currency: the rate to state. The rate on
    /// record for the invoice's date when omitted.
    fx_rate: Option<Decimal>,
    /// The order's terms when omitted.
    payment_terms_id: Option<Uuid>,
    #[serde(default)]
    lines: Vec<InvoiceLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    /// The order line invoiced; or, for a line of the invoice's own, a
    /// charge or an account.
    order_line_id: Option<Uuid>,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    /// The order line's discounts, in proportion to the quantity, when
    /// neither is given; zero for none.
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    /// The charge's tax code when omitted on a charge line.
    tax_code_id: Option<Uuid>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct InvoiceLine {
    id: Uuid,
    line_no: i32,
    order_line_id: Option<Uuid>,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    tax_code_id: Option<Uuid>,
    /// Set when the invoice is posted: the net amount and the tax.
    net_amount: Option<Decimal>,
    tax_amount: Option<Decimal>,
}

async fn invoice_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<InvoiceLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, charge_id, account_id, description, qty, unit_price,
                discount_percent, discount_amount, tax_code_id, net_amount, tax_amount
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
                FieldMeta::new(
                    "sales_order_id",
                    "Sales order",
                    FieldType::document_in(SALES_ORDER, &["open"]),
                )
                .immutable()
                .help("Absent for an invoice without an order (services): it names its customer."),
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                    .help("For an invoice without an order; one of an order is with the order's customer."),
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code")).help(
                    "The order's currency; for an invoice without an order, its own, the \
                     accounting currency when left out.",
                ),
                rate_field(),
                terms_field(
                    "The order's terms when left out. They set when the invoice is due and the \
                     discount for paying it early.",
                ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new(
                        "order_line_id",
                        "Order line",
                        FieldType::line_of("sales_order_id", SALES_ORDER, "lines"),
                    )
                    .help("The order line invoiced; absent for a charge or an account line."),
                ]
                .into_iter()
                .chain(erpcraft_tax::lines::line_kind_fields(
                    "For a line that is not stock and not a charge: the revenue account.",
                ))
                .chain([
                    FieldMeta::new("qty", "Quantity invoiced", FieldType::Decimal)
                        .quantity()
                        .required(),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .currency_of_document("currency")
                        .required(),
                ])
                .chain(erpcraft_tax::lines::discount_fields())
                .chain([
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code")),
                    FieldMeta::new("net_amount", "Net", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                    FieldMeta::new("tax_amount", "Tax", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                ])
                .collect(),
            }],
            alternatives: vec![erpcraft_tax::lines::invoice_line_kinds("lines")],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
            summary: vec![
                FieldMeta::new(
                    "sales_order_id",
                    "Sales order",
                    FieldType::document(SALES_ORDER),
                )
                .readonly(),
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).readonly(),
                FieldMeta::new("customer_name", "Customer name", FieldType::Text)
                    .readonly()
                    .shown_from("customer_id", "name"),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly(),
                FieldMeta::new("total_net", "Net", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly(),
                FieldMeta::new("total_tax", "Tax", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly()
                    .help("Unknown while a draft names a tax code that cannot be worked out yet."),
                FieldMeta::new("total_gross", "Total", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly(),
            ]
            .into_iter()
            .chain(erpcraft_tax::lines::discount_and_charge_fields())
            .chain(accounting_fields())
            .collect(),
            // 1: currency, accounting totals and their declarations.
            // 2: discounts and charges.
            summary_version: 2,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let mut body: InvoiceBody =
            erpcraft_kernel::input::parse(body.clone(), "sales invoice body")?;
        for (index, line) in body.lines.iter().enumerate() {
            erpcraft_tax::lines::check_line(
                ctx,
                doc.company_id,
                index + 1,
                line.order_line_id.is_some(),
                line.charge_id,
                line.account_id,
                line.qty,
                line.unit_price,
                line.discount_percent,
                line.discount_amount,
            )
            .await?;
        }
        match body.sales_order_id {
            Some(order_id) => {
                save_order_ref(ctx, "sales_invoice", doc, order_id, "invoice").await?;
            }
            None => save_direct(ctx, doc, &body).await?,
        }
        // A line of an order takes the order line's discounts unless it
        // gives its own, the amount in proportion to the quantity; a
        // charge line its charge's tax code.
        let ordered: BTreeMap<Uuid, OrderLine> = match body.sales_order_id {
            Some(order_id) => order_lines(&mut *ctx.conn, order_id)
                .await?
                .into_iter()
                .map(|l| (l.id, l))
                .collect(),
            None => BTreeMap::new(),
        };
        let decimals = {
            let (currency, _) = currency_of(&mut *ctx.conn, "sales_invoice", doc).await?;
            erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?
        };
        for line in &mut body.lines {
            if let Some(order_line) = line.order_line_id.and_then(|id| ordered.get(&id))
                && line.discount_percent.is_none()
                && line.discount_amount.is_none()
            {
                line.discount_percent = order_line.discount_percent;
                line.discount_amount = order_line
                    .discount_amount
                    .map(|a| round_money(a * line.qty / order_line.qty, decimals))
                    .filter(|a| !a.is_zero());
            }
            line.discount_percent = line.discount_percent.filter(|p| !p.is_zero());
            line.discount_amount = line.discount_amount.filter(|a| !a.is_zero());
            if let (Some(charge), None) = (line.charge_id, line.tax_code_id) {
                line.tax_code_id =
                    sqlx::query_scalar("SELECT tax_code_id FROM charge WHERE id = $1")
                        .bind(charge)
                        .fetch_one(&mut *ctx.conn)
                        .await?;
            }
        }
        if body
            .fx_rate
            .is_some_and(|rate| rate <= Decimal::ZERO || rate.normalize().scale() > 10)
        {
            return Err(Error::validation(
                "an exchange rate is more than zero, with at most 10 decimal places",
            )
            .with_field("fx_rate"));
        }
        // The order's terms unless the invoice names others.
        sqlx::query(
            "UPDATE sales_invoice i SET fx_rate = $2,
                payment_terms_id = COALESCE($3, (SELECT o.payment_terms_id FROM sales_order o
                                                 WHERE o.document_id = i.sales_order_id),
                                            (SELECT c.payment_terms_id FROM customer c
                                             WHERE c.id = i.customer_id))
             WHERE i.document_id = $1",
        )
        .bind(doc.id)
        .bind(body.fx_rate)
        .bind(body.payment_terms_id)
        .execute(&mut *ctx.conn)
        .await?;
        let ids: Vec<Uuid> = body.lines.iter().filter_map(|l| l.order_line_id).collect();
        if let Some(order_id) = body.sales_order_id {
            check_order_lines(ctx, order_id, &ids).await?;
        }

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "sales_invoice_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_invoice_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            let column = |f: fn(&InvoiceLineInput) -> Option<Uuid>| {
                body.lines.iter().map(f).collect::<Vec<_>>()
            };
            let amounts = |f: fn(&InvoiceLineInput) -> Option<Decimal>| {
                body.lines.iter().map(f).collect::<Vec<_>>()
            };
            sqlx::query(
                "INSERT INTO sales_invoice_line (id, document_id, line_no, order_line_id, qty,
                    unit_price, tax_code_id, charge_id, account_id, description, discount_percent,
                    discount_amount)
                 SELECT r.id, $1, r.line_no, r.order_line_id, r.qty, r.unit_price, r.tax_code_id,
                    r.charge_id, r.account_id, NULLIF(trim(r.description), ''),
                    r.discount_percent, r.discount_amount
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[], $6::numeric[],
                             $7::uuid[], $8::uuid[], $9::uuid[], $10::text[], $11::numeric[],
                             $12::numeric[])
                    AS r (id, line_no, order_line_id, qty, unit_price, tax_code_id, charge_id,
                          account_id, description, discount_percent, discount_amount)",
            )
            .bind(doc.id)
            .bind(&line_ids)
            .bind(&line_nos)
            .bind(column(|l| l.order_line_id))
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.unit_price)
                    .collect::<Vec<_>>(),
            )
            .bind(column(|l| l.tax_code_id))
            .bind(column(|l| l.charge_id))
            .bind(column(|l| l.account_id))
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.description.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(amounts(|l| l.discount_percent))
            .bind(amounts(|l| l.discount_amount))
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The order and its customer, and what the invoice comes to: net, tax
    /// and both together. Each line is worked out as posting does it: the
    /// quantity times the price, rounded, and the tax on that. Once posted,
    /// the tax is the tax that was recorded.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        use document::body::{decimal, uuid};
        let mut summary = follows(conn, body).await?;
        let base = company::base_currency(conn, doc.company_id).await?;
        let currency = body["currency"]
            .as_str()
            .map_or(base.clone(), str::to_owned);
        let conversion = (currency != base).then(|| erpcraft_tax::Conversion {
            currency: currency.clone(),
            rate: decimal(&body["fx_rate"]).unwrap_or(Decimal::ONE),
        });
        let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
        let mut net = Decimal::ZERO;
        let mut tax = Some(Decimal::ZERO);
        for line in document::body::lines(body, "lines") {
            let charged = decimal(&line["net_amount"])
                .unwrap_or_else(|| erpcraft_tax::lines::body_line_net(line, decimals));
            net += charged;
            let line_id = uuid(&line["id"]);
            let on_line = match (uuid(&line["tax_code_id"]), decimal(&line["tax_amount"])) {
                (None, _) => Some(Decimal::ZERO),
                (Some(_), Some(recorded)) => Some(recorded),
                (Some(code), None) if doc.status == "draft" => erpcraft_tax::estimate(
                    &mut *conn,
                    doc.company_id,
                    code,
                    Direction::Collected,
                    charged,
                    line_id,
                    conversion.as_ref(),
                )
                .await
                .ok()
                .map(|taxes| erpcraft_tax::Tax::total(&taxes)),
                (Some(_), None) => None,
            };
            tax = match (tax, on_line) {
                (Some(tax), Some(on_line)) => Some(tax + on_line),
                _ => None,
            };
        }
        summary["currency"] = json!(currency);
        summary["total_net"] = json!(net);
        summary["total_tax"] = json!(tax);
        summary["total_gross"] = json!(tax.map(|tax| net + tax));
        // Charges: lines of the invoice's own that are charges, and lines
        // that follow a charge on the order.
        let lines = document::body::lines(body, "lines");
        let followed: Vec<Uuid> = lines
            .iter()
            .filter_map(|l| document::body::uuid(&l["order_line_id"]))
            .collect();
        let charge_lines: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM sales_order_line WHERE id = ANY($1) AND charge_id IS NOT NULL",
        )
        .bind(&followed)
        .fetch_all(&mut *conn)
        .await?;
        let (discount, charges) =
            erpcraft_tax::lines::discount_and_charges(lines, decimals, |line| {
                !line["charge_id"].is_null()
                    || document::body::uuid(&line["order_line_id"])
                        .is_some_and(|id| charge_lines.contains(&id))
            });
        summary["total_discount"] = json!(discount);
        summary["total_charges"] = json!(charges);
        let totals = accounting_totals(conn, doc, conversion.is_some(), net, tax).await?;
        add_totals(&mut summary, totals);
        Ok(summary)
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (order, customer): (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
            "SELECT sales_order_id, customer_id FROM sales_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        let (currency, fx_rate) = currency_of(conn, "sales_invoice", doc).await?;
        let terms: Option<Uuid> =
            sqlx::query_scalar("SELECT payment_terms_id FROM sales_invoice WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *conn)
                .await?;
        Ok(json!({
            "sales_order_id": order,
            "customer_id": customer,
            "currency": currency,
            "fx_rate": fx_rate,
            "payment_terms_id": terms,
            "lines": invoice_lines(conn, doc.id).await?,
        }))
    }
}

/// Store the header of a direct invoice: its customer and currency. Its
/// lines are its own: none may follow an order line.
async fn save_direct(ctx: &mut Ctx<'_>, doc: &Document, body: &InvoiceBody) -> Result<()> {
    let customer = body.customer_id.ok_or_else(|| {
        Error::validation("an invoice without a sales order names its customer")
            .with_field("customer_id")
    })?;
    if let Some(index) = body.lines.iter().position(|l| l.order_line_id.is_some()) {
        return Err(Error::line(
            "lines",
            index + 1,
            Some("order_line_id"),
            "an invoice without a sales order has no order lines: give a charge or an account",
        ));
    }
    let existing: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
            .bind(doc.id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    if existing.flatten().is_some() {
        return Err(Error::validation(
            "the sales order of a document cannot change; create a new document instead",
        ));
    }
    let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customer WHERE id = $1)")
        .bind(customer)
        .fetch_one(&mut *ctx.conn)
        .await?;
    if !known {
        return Err(Error::validation("the customer does not exist").with_field("customer_id"));
    }
    let base = company::base_currency(&mut *ctx.conn, doc.company_id).await?;
    let currency = body.currency.clone().filter(|c| *c != base);
    if let Some(currency) = &currency {
        erpcraft_gl::currency_decimals(&mut *ctx.conn, currency)
            .await
            .map_err(|e| e.with_field("currency"))?;
    }
    sqlx::query(
        "INSERT INTO sales_invoice (document_id, customer_id, currency) VALUES ($1, $2, $3)
         ON CONFLICT (document_id) DO UPDATE SET customer_id = EXCLUDED.customer_id,
            currency = EXCLUDED.currency",
    )
    .bind(doc.id)
    .bind(customer)
    .bind(currency)
    .execute(&mut *ctx.conn)
    .await?;
    Ok(())
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
        .from_states(&["draft"])
        .permission("sales_invoice:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_INVOICE, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} invoice cannot be posted",
                doc.status
            )));
        }
        let order_id: Option<Uuid> =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        // An invoice of an order, or a direct one with lines of its own.
        let (mut order, mut order_lines) = match order_id {
            Some(order_id) => {
                let (order, lines) = lock_order(ctx, order_id).await?;
                (Some(order), lines)
            }
            None => (None, BTreeMap::new()),
        };
        if let Some(order) = &order {
            check_open(
                ctx,
                order.company_id,
                &order.status,
                order_lines.values(),
                SALES_INVOICE,
                "only an open order can be invoiced",
            )
            .await?;
            check_customer(ctx, order, SALES_INVOICE).await?;
        }
        let lines = invoice_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("an invoice needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        // Charged in the order's currency at the invoice's rate: revenue,
        // receivables and tax alike. Cost of goods sold was posted by the
        // delivery, in the accounting currency, from the stock ledger.
        let (currency, stated) = currency_of(&mut *ctx.conn, "sales_invoice", &doc).await?;
        let rate = erpcraft_gl::document_rate(
            ctx,
            doc.company_id,
            &currency,
            doc.doc_date,
            stated,
            SALES_INVOICE,
        )
        .await?;
        let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?;
        let base_decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        let conversion = (currency
            != company::base_currency(&mut *ctx.conn, doc.company_id).await?)
            .then(|| erpcraft_tax::Conversion {
                currency: currency.clone(),
                rate,
            });

        let price_differs = ctx.policy(policy::PRICE_DIFFERS, doc.company_id).await?;
        let mut owed = Decimal::ZERO;
        let mut owed_value = Decimal::ZERO;
        let mut tax_amounts = vec![];
        let mut net_amounts = vec![];
        let mut taxes = vec![];
        let mut gl = vec![];
        for line in &lines {
            let charged = erpcraft_tax::line_net(
                line.qty,
                line.unit_price,
                line.discount_percent,
                line.discount_amount,
                decimals,
            )
            .ok_or_else(|| {
                Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("discount_amount"),
                    "the discounts come to more than the line",
                )
            })?;
            // Revenue: the sales account for goods, the line's own account
            // for a charge or an account line.
            let (charge_id, account_id) = match line.order_line_id {
                Some(id) => order_lines
                    .get(&id)
                    .map(|o| (o.charge_id, o.account_id))
                    .unwrap_or_default(),
                None => (line.charge_id, line.account_id),
            };
            let revenue = revenue_account(
                &mut *ctx.conn,
                doc.company_id,
                accounts.revenue_account_id,
                charge_id,
                account_id,
            )
            .await?;
            if let Some(order_line_id) = line.order_line_id {
                let ordered = order_lines
                    .get_mut(&order_line_id)
                    .ok_or_else(|| Error::internal("invoice line without its order line"))?;
                let facts = policy::price_differs(ordered.unit_price, line.unit_price);
                if let Decision::Refused(refusal) = price_differs.evaluate(facts)? {
                    let why = refusal.message.unwrap_or_else(|| {
                    format!(
                        "the invoice price {} differs too much from the price {} of order line {}",
                        trim(line.unit_price),
                        trim(ordered.unit_price),
                        ordered.line_no
                    )
                });
                    return Err(Error::line(
                        "lines",
                        line.line_no as usize,
                        Some("unit_price"),
                        why,
                    ));
                }
                let open_qty = ordered.qty_delivered - ordered.qty_invoiced;
                if line.qty > open_qty {
                    return Err(Error::line(
                        "lines",
                        line.line_no as usize,
                        Some("qty"),
                        format_args!(
                            "{} have been delivered and not yet invoiced on order line {}, not {}",
                            trim(open_qty),
                            ordered.line_no,
                            trim(line.qty)
                        ),
                    ));
                }
                ordered.qty_invoiced += line.qty;
            }
            net_amounts.push(Some(charged));
            owed += charged;
            owed_value += round_money(charged * rate, base_decimals);
            // Tax comes on top of the net amount and is owed with it.
            if let Some(code) = line.tax_code_id {
                // One tax per component of a group, each to its own account.
                let line_taxes = erpcraft_tax::quote(
                    ctx,
                    doc.company_id,
                    code,
                    Direction::Collected,
                    charged,
                    Some(line.id),
                    conversion.as_ref(),
                )
                .await?;
                tax_amounts.push(Some(erpcraft_tax::Tax::total(&line_taxes)));
                for tax in line_taxes {
                    if !tax.amount.is_zero() {
                        owed += tax.amount;
                        owed_value += tax.value();
                        gl.push(in_currency(
                            gl_line(
                                accounts.receivable_account_id,
                                tax.amount,
                                Decimal::ZERO,
                                line.id,
                            ),
                            conversion.as_ref(),
                        ));
                        // The tax account holds the accounting value.
                        gl.push(gl_line(tax.account_id, Decimal::ZERO, tax.value(), line.id));
                    }
                    taxes.push(tax);
                }
            } else {
                tax_amounts.push(None);
            }
            if !charged.is_zero() {
                gl.push(in_currency(
                    gl_line(
                        accounts.receivable_account_id,
                        charged,
                        Decimal::ZERO,
                        line.id,
                    ),
                    conversion.as_ref(),
                ));
                gl.push(in_currency(
                    gl_line(revenue, Decimal::ZERO, charged, line.id),
                    conversion.as_ref(),
                ));
            }
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "sales_invoice_line",
            &ids,
            &[("tax_amount", &tax_amounts), ("net_amount", &net_amounts)],
        )
        .await?;

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        // What is now owed, as one open item for a payment to settle.
        if !owed.is_zero() {
            let (party_id, terms): (Uuid, Option<Uuid>) = sqlx::query_as(
                "SELECT COALESCE(i.customer_id, o.customer_id), i.payment_terms_id
                 FROM sales_invoice i
                 LEFT JOIN sales_order o ON o.document_id = i.sales_order_id
                 WHERE i.document_id = $1",
            )
            .bind(doc.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            erpcraft_openitems::open(
                &mut *ctx.conn,
                &posting,
                &NewOpenItem {
                    kind: Kind::Receivable,
                    party_type: "customer",
                    party_id,
                    account_id: accounts.receivable_account_id,
                    amount: owed_value,
                    // Without terms, due on its date.
                    due_date: Some(posting.posting_date),
                    terms,
                    foreign: conversion.as_ref().map(|c| (c.currency.clone(), owed)),
                },
            )
            .await?;
        }
        if conversion.is_some() {
            sqlx::query("UPDATE sales_invoice SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }
        if let Some(order) = &mut order {
            settle_order(ctx, order, &order_lines, false).await?;
        }
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
        .from_states(&["posted"])
        .destructive()
        .permission("sales_invoice:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_INVOICE, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} invoice cannot be reversed",
                doc.status
            )));
        }
        let order_id: Option<Uuid> =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
                .bind(doc.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        // An invoice of an order, or a direct one with lines of its own.
        let (mut order, mut order_lines) = match order_id {
            Some(order_id) => {
                let (order, lines) = lock_order(ctx, order_id).await?;
                (Some(order), lines)
            }
            None => (None, BTreeMap::new()),
        };
        if returns::is_credited(&mut *ctx.conn, doc.id).await? {
            return Err(Error::state(
                "this invoice has been credited; reverse the credit note first",
            ));
        }
        for line in invoice_lines(&mut *ctx.conn, doc.id).await? {
            // A charge or an account line of the invoice's own is not on
            // the order.
            let Some(order_line_id) = line.order_line_id else {
                continue;
            };
            let ordered = order_lines
                .get_mut(&order_line_id)
                .ok_or_else(|| Error::internal("invoice line without its order line"))?;
            if line.qty > ordered.qty_invoiced {
                return Err(Error::state(format!(
                    "line {}: goods on this order line have come back and been credited since; reverse the credit note first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
            ordered.qty_invoiced -= line.qty;
        }

        let original = document::original_posting(&mut *ctx.conn, &doc).await?;
        let memo = input
            .memo
            .or_else(|| doc.number.as_ref().map(|n| format!("Reversal of {n}")));
        document::reverse_posting(ctx, original.id, input.posting_date, memo).await?;
        if let Some(order) = &mut order {
            settle_order(ctx, order, &order_lines, true).await?;
        }
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}
