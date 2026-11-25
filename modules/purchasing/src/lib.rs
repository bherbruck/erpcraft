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
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, DynCommand,
    EntityMeta, Error, FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection, Result,
    StateMeta, async_trait, command, company, round_money, schemars,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use erpcraft_stock::{Cost, LineLot, StockLine, lots};
use erpcraft_tax::Direction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod credit;
mod policy;
mod returns;

const PURCHASE_ORDER: &str = "purchase_order";
const PURCHASE_RECEIPT: &str = "purchase_receipt";
const VENDOR_INVOICE: &str = "vendor_invoice";

pub struct PurchasingModule;

impl Module for PurchasingModule {
    fn name(&self) -> &'static str {
        "purchasing"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "stock", "open_items", "tax", "pricing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "purchasing",
                sql: include_str!("../migrations/0001_purchasing.sql"),
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
                name: "over_receipt",
                sql: include_str!("../migrations/0004_over_receipt.sql"),
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
            Migration {
                version: 10,
                name: "allocate_by_weight",
                sql: include_str!("../migrations/0010_allocate_by_weight.sql"),
            },
            Migration {
                version: 11,
                name: "addresses",
                sql: include_str!("../migrations/0011_addresses.sql"),
            },
            Migration {
                version: 12,
                name: "credit",
                sql: include_str!("../migrations/0012_credit.sql"),
            },
            Migration {
                version: 13,
                name: "rate_precision",
                sql: include_str!("../migrations/0013_rate_precision.sql"),
            },
            Migration {
                version: 14,
                name: "list_indexes",
                sql: include_str!("../migrations/0014_list_indexes.sql"),
            },
            Migration {
                version: 15,
                name: "units",
                sql: include_str!("../migrations/0015_units.sql"),
            },
        ]
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
                .field(
                    FieldMeta::new(
                        "payment_terms_id",
                        "Payment terms",
                        FieldType::link("payment_terms"),
                    )
                    .help("The vendor's usual terms: what its orders and invoices start with."),
                )
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("purchasing_settings", "Purchasing settings")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
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
            Arc::new(returns::PurchaseReturn),
            Arc::new(returns::VendorCreditNote),
            Arc::new(credit::VendorCredit),
        ]
    }

    fn policies(&self) -> Vec<erpcraft_kernel::PolicyPoint> {
        policy::points()
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
            command(Follow {
                name: "purchase_order.receive",
                permission: "purchase_receipt:create",
                description: "Draft a receipt for an open purchase order, with a line for every \
                              order line and the quantity still to be received on it. Nothing is \
                              posted: change the quantities to what arrived, then post the \
                              receipt with purchase_receipt.post.",
                doc_type: PURCHASE_RECEIPT,
            }),
            command(Follow {
                name: "purchase_order.invoice",
                permission: "vendor_invoice:create",
                description: "Draft a vendor invoice for an open purchase order, with a line for \
                              everything received and not yet invoiced, at the price of the \
                              order. Nothing is posted: set the prices, tax codes and the \
                              vendor's invoice number, then post it with vendor_invoice.post.",
                doc_type: VENDOR_INVOICE,
            }),
            command(PostReceipt),
            command(ReverseReceipt),
            command(PostInvoice),
            command(ReverseInvoice),
            command(returns::DraftReturn),
            command(returns::PostReturn),
            command(returns::ReverseReturn),
            command(returns::DraftCreditNote),
            command(returns::PostCreditNote),
            command(returns::ReverseCreditNote),
            command(returns::AwaitingCredit),
            command(credit::PostCredit),
            command(credit::ReverseCredit),
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
    grni_account_id: Uuid,
    payable_account_id: Uuid,
    price_variance_account_id: Uuid,
}

async fn settings(conn: &mut PgConnection, company_id: Uuid) -> Result<Settings> {
    sqlx::query_as(
        "SELECT grni_account_id, payable_account_id, price_variance_account_id
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

/// One line of an order with how far it has got. Quantities and values
/// received and invoiced are net: what was returned has left
/// `qty_received` and `received_value`, and what was credited for returned
/// goods has left `qty_invoiced` and `cleared_value`. The returns and
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
    /// The unit of the quantity and price; the factor is how many stock
    /// units one of it is.
    uom: Option<String>,
    factor: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    /// A line that is not stock counts as received in full: invoicing it
    /// is what completes it.
    qty_received: Decimal,
    #[serde(skip)]
    received_value: Decimal,
    qty_invoiced: Decimal,
    #[serde(skip)]
    cleared_value: Decimal,
    /// Sent back to the vendor.
    qty_returned: Decimal,
    #[serde(skip)]
    returned_value: Decimal,
    /// Of what was returned, credited by the vendor.
    qty_credited: Decimal,
    #[serde(skip)]
    credited_value: Decimal,
    /// The line's net amount: shown when the order is read.
    #[sqlx(default)]
    net_amount: Option<Decimal>,
}

impl OrderLine {
    /// `qty` of this line's unit in stock units, exactly.
    fn in_stock_units(&self, qty: Decimal, collection_line: i32) -> Result<Decimal> {
        erpcraft_stock::units::to_stock_or(qty, self.factor, self.uom.as_deref().unwrap_or(""))
            .map_err(|e| e.with_line("lines", collection_line as usize))
    }

    /// The item and warehouse of a stock line; refused for a line that is
    /// not stock, which nothing is received or returned of.
    fn stock(&self, collection_line: i32) -> Result<(Uuid, Uuid)> {
        self.item_id.zip(self.warehouse_id).ok_or_else(|| {
            Error::line(
                "lines",
                collection_line as usize,
                Some("order_line_id"),
                format_args!(
                    "order line {} is not stock: nothing is received or returned of it",
                    self.line_no
                ),
            )
        })
    }

    /// The line's net amount in the order's currency.
    fn net(&self, decimals: u32) -> Decimal {
        erpcraft_tax::line_net(
            self.qty,
            self.unit_price,
            self.discount_percent,
            self.discount_amount,
            decimals,
        )
        .unwrap_or_default()
    }
}

impl OrderLine {
    /// Everything ordered has arrived, and everything that arrived has
    /// been invoiced. Where the company's policy let more arrive than was
    /// ordered, the extra goods must be invoiced too before the line is
    /// complete.
    fn complete(&self) -> bool {
        self.qty_received >= self.qty && self.qty_invoiced == self.qty_received
    }
}

async fn order_lines(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<OrderLine>> {
    let lines = sqlx::query_as(
        "SELECT l.id, l.line_no, l.item_id, l.warehouse_id, l.charge_id, l.account_id,
                l.description, l.qty, l.uom, l.factor, l.unit_price, l.discount_percent,
                l.discount_amount,
                CASE WHEN l.item_id IS NULL THEN l.qty
                     ELSE COALESCE(p.qty_received - p.qty_returned, 0) END AS qty_received,
                COALESCE(p.received_value - p.returned_value, 0) AS received_value,
                COALESCE(p.qty_invoiced - p.qty_credited, 0) AS qty_invoiced,
                COALESCE(p.cleared_value - p.credited_value, 0) AS cleared_value,
                COALESCE(p.qty_returned, 0) AS qty_returned,
                COALESCE(p.returned_value, 0) AS returned_value,
                COALESCE(p.qty_credited, 0) AS qty_credited,
                COALESCE(p.credited_value, 0) AS credited_value
         FROM purchase_order_line l
         LEFT JOIN purchase_order_progress p ON p.line_id = l.id
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
        "INSERT INTO purchase_order_progress
            (line_id, qty_received, received_value, qty_invoiced, cleared_value,
             qty_returned, returned_value, qty_credited, credited_value)
         SELECT * FROM unnest($1::uuid[], $2::numeric[], $3::numeric[], $4::numeric[],
                              $5::numeric[], $6::numeric[], $7::numeric[], $8::numeric[],
                              $9::numeric[])
         ON CONFLICT (line_id) DO UPDATE SET
            qty_received = EXCLUDED.qty_received,
            received_value = EXCLUDED.received_value,
            qty_invoiced = EXCLUDED.qty_invoiced,
            cleared_value = EXCLUDED.cleared_value,
            qty_returned = EXCLUDED.qty_returned,
            returned_value = EXCLUDED.returned_value,
            qty_credited = EXCLUDED.qty_credited,
            credited_value = EXCLUDED.credited_value",
    )
    .bind(lines.iter().map(|l| l.id).collect::<Vec<_>>())
    // Stored as received and invoiced in all, returns and credits apart.
    .bind(column(|l| l.qty_received + l.qty_returned))
    .bind(column(|l| l.received_value + l.returned_value))
    .bind(column(|l| l.qty_invoiced + l.qty_credited))
    .bind(column(|l| l.cleared_value + l.credited_value))
    .bind(column(|l| l.qty_returned))
    .bind(column(|l| l.returned_value))
    .bind(column(|l| l.qty_credited))
    .bind(column(|l| l.credited_value))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// After a reversal has taken back what its document did to an order line:
/// whether the line still holds together. A reversal restores the values
/// its document stored, but the document that levelled the line (as much
/// kept as invoiced) took whatever value was left then; undone out of
/// order, the line could be level and still hold value in GRNI, which the
/// database refuses. The reversal is refused here instead, in words.
fn check_still_level(line: &OrderLine, document_line: i32) -> Result<()> {
    let left = line.received_value - line.cleared_value;
    let negative = [
        line.received_value,
        line.cleared_value,
        line.returned_value,
        line.credited_value,
    ]
    .iter()
    .any(|v| v.is_sign_negative() && !v.is_zero());
    if negative || (line.qty_received == line.qty_invoiced && !left.is_zero()) {
        return Err(Error::state(format!(
            "line {document_line}: taking this back now would leave order line {} out of balance \
             in goods received not invoiced; take back the returns and credit notes posted after \
             it on that order line first, the latest first",
            line.line_no
        ))
        .with_line("lines", document_line as usize));
    }
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
    // reversal leaves it with goods received and not invoiced, which a
    // closed order otherwise does not hold. An order someone closed short
    // stays closed otherwise, and so does a closed order the company's
    // policy let a receipt or an invoice post against
    // (`purchase_order.closed`): posting never reopens an order.
    let complete = lines.values().all(OrderLine::complete);
    let unbilled = lines.values().any(|l| l.qty_invoiced != l.qty_received);
    match order.status.as_str() {
        "open" if complete => document::transition(ctx, order, "closed").await,
        "closed" if unbilled && reversal => document::transition(ctx, order, "open").await,
        _ => Ok(()),
    }
}

/// Whether a receipt or an invoice (`doc_type`) may refer to an order in
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
                    .map(|l| (l.qty, l.qty_received, l.qty_invoiced)),
            );
            let ruling = ctx
                .policy(policy::CLOSED_ORDER, company_id)
                .await?
                .ruling(facts)?;
            match ruling.decision {
                Decision::Allowed => Ok(()),
                Decision::Refused(refusal) => {
                    Err(Error::state(refusal.message.unwrap_or_else(|| {
                        format!("the purchase order is closed; {refused}")
                    })))
                }
            }
        }
        status => Err(Error::state(format!(
            "the purchase order is {status}; {refused}"
        ))),
    }
}

/// Ask the company's policy whether this (`doc_type`) may be done for the
/// order's vendor, active or not.
async fn check_vendor(ctx: &mut Ctx<'_>, order: &Document, doc_type: &str) -> Result<()> {
    let (code, active): (String, bool) = sqlx::query_as(
        "SELECT v.code, v.active FROM purchase_order o JOIN vendor v ON v.id = o.vendor_id
         WHERE o.document_id = $1",
    )
    .bind(order.id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    let facts = policy::inactive_vendor(doc_type, &code, active);
    ctx.decide(policy::INACTIVE_VENDOR, order.company_id, facts)
        .await
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
    // An approved order's lines can no longer change, so documents may
    // refer to them.
    let Some(status) = status else {
        return Err(Error::validation(
            "purchase_order_id must be a purchase order of the document's company",
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

/// The vendor of an order, for the summary of the order and of the
/// documents that follow from it.
async fn party_of(conn: &mut PgConnection, order_id: Uuid) -> Result<Option<(Uuid, String)>> {
    let party = sqlx::query_as(
        "SELECT p.id, p.name FROM purchase_order o JOIN vendor p ON p.id = o.vendor_id
         WHERE o.document_id = $1",
    )
    .bind(order_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(party)
}

/// The header of the summary of a document that follows from an order:
/// the order its body names, and the order's vendor.
async fn follows(conn: &mut PgConnection, body: &Value) -> Result<Value> {
    let order = document::body::uuid(&body["purchase_order_id"]);
    let party = match (order, document::body::uuid(&body["vendor_id"])) {
        (Some(order), _) => party_of(conn, order).await?,
        (None, Some(vendor)) => {
            sqlx::query_as("SELECT id, name FROM vendor WHERE id = $1")
                .bind(vendor)
                .fetch_optional(&mut *conn)
                .await?
        }
        (None, None) => None,
    };
    Ok(json!({
        "purchase_order_id": order,
        "vendor_id": party.as_ref().map(|(id, _)| id),
        "vendor_name": party.as_ref().map(|(_, name)| name),
    }))
}

/// A document line for [`gl_scopes`]: its id, item, warehouse and charge.
pub(crate) type LineAbout = (Uuid, Option<Uuid>, Option<Uuid>, Option<Uuid>);

/// What each document line is about, for the general ledger's dimension
/// defaults: its warehouse and the places above it, its item, its charge.
/// `lines` gives each line's id, item, warehouse and charge.
pub(crate) async fn gl_scopes(
    conn: &mut PgConnection,
    lines: &[LineAbout],
) -> Result<std::collections::HashMap<Uuid, Vec<(&'static str, Uuid)>>> {
    let places =
        erpcraft_stock::locations(conn, lines.iter().filter_map(|(_, _, w, _)| *w)).await?;
    Ok(lines
        .iter()
        .map(|&(id, item, warehouse, charge)| {
            let mut scopes = match warehouse {
                Some(warehouse) => places.scopes(warehouse, item),
                None => item.map(|item| vec![("item", item)]).unwrap_or_default(),
            };
            if let Some(charge) = charge {
                scopes.push(("charge", charge));
            }
            (id, scopes)
        })
        .collect())
}

/// The vendor of a purchase order.
pub(crate) async fn order_vendor(conn: &mut PgConnection, order_id: Uuid) -> Result<Uuid> {
    Ok(
        sqlx::query_scalar("SELECT vendor_id FROM purchase_order WHERE document_id = $1")
            .bind(order_id)
            .fetch_one(&mut *conn)
            .await?,
    )
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
        ..GlLine::default()
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
    /// The accounting currency when omitted.
    currency: Option<String>,
    /// The vendor's terms when omitted.
    payment_terms_id: Option<Uuid>,
    /// One of the vendor's addresses, the one it sells from; its default
    /// billing address when omitted.
    billing_address_id: Option<Uuid>,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    /// A stock line: the item and the warehouse it is received into.
    item_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    /// A line that is not stock: a charge or an account, and what it is.
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    /// The unit of the quantity and price: one of the item's purchase
    /// units, or its stock unit when omitted.
    uom: Option<String>,
    /// Left out on an item line: what the vendor's price lists propose,
    /// else the last price paid to it.
    unit_price: Option<Decimal>,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
}

/// The item lines of an order as the price check reads them.
async fn price_lines(
    conn: &mut PgConnection,
    lines: &[OrderLine],
) -> Result<Vec<erpcraft_pricing::LinePrice>> {
    let mut out = vec![];
    for line in lines {
        let Some(item) = line.item_id else { continue };
        let code: String = sqlx::query_scalar("SELECT code FROM item WHERE id = $1")
            .bind(item)
            .fetch_one(&mut *conn)
            .await?;
        out.push(erpcraft_pricing::LinePrice {
            line_id: line.id,
            line_no: line.line_no,
            item_code: code,
            unit_price: line.unit_price,
            discount_percent: line.discount_percent,
        });
    }
    Ok(out)
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
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code")).help(
                    "The currency of the prices, and of the invoices that follow. The company's \
                     accounting currency when left out; fixed once the order is approved.",
                ),
                terms_field("The vendor's usual terms when left out; what its invoices start with."),
                vendor_address_field(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item"))
                        .help("For a stock line, with the warehouse it is received into."),
                    FieldMeta::new("warehouse_id", "Deliver to", FieldType::link("warehouse")),
                ]
                .into_iter()
                .chain(erpcraft_tax::lines::line_kind_fields(
                    "For a line that is not stock and not a charge: the expense or asset account.",
                ))
                .chain([
                    FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .required(),
                    FieldMeta::new("uom", "Unit", FieldType::Text).help(
                        "One of the item's purchase units (item_uom), or its stock unit when \
                         left out; the quantity and the price are per this unit. A line that is \
                         not stock may name any unit.",
                    ),
                    FieldMeta::new("factor", "Stock units in one", FieldType::Decimal)
                        .quantity()
                        .readonly()
                        .help("How many of the item's stock unit one of the line's unit is."),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .currency_of_document("currency")
                        .help("Left out on an item line: what the vendor's price lists propose, else the last price paid."),
                ])
                .chain(erpcraft_tax::lines::discount_fields())
                .chain([
                    FieldMeta::new("net_amount", "Net", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                    FieldMeta::new("qty_received", "Received", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly()
                        .help("Net of what was returned."),
                    FieldMeta::new("qty_invoiced", "Invoiced", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly()
                        .help("Net of what was credited for returned goods."),
                    FieldMeta::new("qty_returned", "Returned", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly(),
                    FieldMeta::new("qty_credited", "Credited", FieldType::Decimal)
                        .quantity()
                        .unit_of("item_id", "uom")
                        .readonly()
                        .help("Of what was returned, what the vendor has credited."),
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
                // Reversing the receipt or invoice that completed it.
                ("closed".into(), "open".into()),
            ],
            summary: vec![
                FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor")).readonly(),
                FieldMeta::new("vendor_name", "Vendor name", FieldType::Text)
                    .readonly()
                    .shown_from("vendor_id", "name"),
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
        let mut body: OrderBody =
            erpcraft_kernel::input::parse(body.clone(), "purchase order body")?;
        // What the vendor's price lists (else the last price paid) propose
        // for each item line: a line without a price takes it, and its
        // discount unless it gives one.
        let base = company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let order_currency = body.currency.clone().unwrap_or_else(|| base.clone());
        // Each stock line's unit, and how many stock units one of it is.
        let mut units: Vec<(Option<String>, Decimal)> = vec![];
        for (index, line) in body.lines.iter().enumerate() {
            units.push(match line.item_id {
                Some(item) => {
                    let (uom, factor) = erpcraft_stock::units::unit(
                        &mut *ctx.conn,
                        item,
                        line.uom.as_deref(),
                        erpcraft_stock::units::Purpose::Purchase,
                    )
                    .await
                    .map_err(|e| e.with_line("lines", index + 1))?;
                    erpcraft_stock::units::to_stock_or(line.qty, factor, &uom)
                        .map_err(|e| e.with_line("lines", index + 1))?;
                    (Some(uom), factor)
                }
                None => (
                    line.uom
                        .as_deref()
                        .map(str::trim)
                        .filter(|u| !u.is_empty())
                        .map(str::to_owned),
                    Decimal::ONE,
                ),
            });
        }
        let mut proposals = vec![];
        for (index, line) in body.lines.iter_mut().enumerate() {
            let proposal = match line.item_id {
                Some(item) => {
                    erpcraft_pricing::propose(
                        &mut *ctx.conn,
                        &erpcraft_pricing::Query {
                            company_id: doc.company_id,
                            side: erpcraft_pricing::Side::Purchase,
                            party_id: body.vendor_id,
                            item_id: item,
                            // Lists are per stock unit, breaks too.
                            qty: line.qty * units[index].1,
                            currency: order_currency.clone(),
                            date: doc.doc_date,
                        },
                    )
                    .await?
                    .map(|p| p.per(units[index].1))
                }
                None => None,
            };
            if line.unit_price.is_none() {
                line.unit_price = proposal.as_ref().and_then(|p| p.unit_price);
                if line.unit_price.is_some() && line.discount_percent.is_none() {
                    line.discount_percent = proposal.as_ref().and_then(|p| p.discount_percent);
                }
            }
            if line.unit_price.is_none() {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("unit_price"),
                    "give a unit price: no price list has one for this line, and nothing was \
                     paid for it before",
                ));
            }
            proposals.push(proposal);
        }
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
                line.unit_price.unwrap_or_default(),
                line.discount_percent,
                line.discount_amount,
            )
            .await?;
        }
        for line in &mut body.lines {
            line.discount_percent = erpcraft_tax::lines::no_zero(line.discount_percent);
            line.discount_amount = erpcraft_tax::lines::no_zero(line.discount_amount);
        }
        let warehouses: Vec<Uuid> = body.lines.iter().filter_map(|l| l.warehouse_id).collect();
        if !company::owns_all(&mut *ctx.conn, "warehouse", &warehouses, doc.company_id).await? {
            return Err(Error::validation(
                "every line needs a warehouse of the document's company",
            ));
        }

        let currency = body.currency.filter(|c| *c != base);
        if let Some(currency) = &currency {
            erpcraft_gl::currency_decimals(&mut *ctx.conn, currency)
                .await
                .map_err(|e| e.with_field("currency"))?;
        }
        if let Some(address) = body.billing_address_id {
            erpcraft_party::check_address(
                &mut *ctx.conn,
                "vendor",
                body.vendor_id,
                address,
                "billing_address_id",
            )
            .await?;
        }
        // The vendor's terms unless the order names others.
        sqlx::query(
            "INSERT INTO purchase_order (document_id, vendor_id, currency, payment_terms_id,
                 billing_address_id)
             VALUES ($1, $2, $3,
                     COALESCE($4, (SELECT payment_terms_id FROM vendor WHERE id = $2)), $5)
             ON CONFLICT (document_id) DO UPDATE SET vendor_id = EXCLUDED.vendor_id,
                currency = EXCLUDED.currency, payment_terms_id = EXCLUDED.payment_terms_id,
                billing_address_id = EXCLUDED.billing_address_id",
        )
        .bind(doc.id)
        .bind(body.vendor_id)
        .bind(currency)
        .bind(body.payment_terms_id)
        .bind(body.billing_address_id)
        .execute(&mut *ctx.conn)
        .await?;
        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "purchase_order_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM purchase_order_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        let kept: Vec<(Uuid, erpcraft_pricing::Proposal)> = line_ids
            .iter()
            .zip(proposals)
            .filter_map(|(id, p)| p.map(|p| (*id, p)))
            .collect();
        erpcraft_pricing::keep(&mut *ctx.conn, doc.id, &kept).await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO purchase_order_line (id, document_id, line_no, item_id, warehouse_id,
                    qty, unit_price, charge_id, account_id, description, discount_percent,
                    discount_amount, uom, factor)
                 SELECT r.id, $1, r.line_no, r.item_id, r.warehouse_id, r.qty, r.unit_price,
                    r.charge_id, r.account_id, NULLIF(trim(r.description), ''),
                    r.discount_percent, r.discount_amount, r.uom, r.factor
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::uuid[], $6::numeric[],
                             $7::numeric[], $8::uuid[], $9::uuid[], $10::text[], $11::numeric[],
                             $12::numeric[], $13::text[], $14::numeric[])
                    AS r (id, line_no, item_id, warehouse_id, qty, unit_price, charge_id,
                          account_id, description, discount_percent, discount_amount, uom,
                          factor)",
            )
            .bind(doc.id)
            .bind(&line_ids)
            .bind(&line_nos)
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.item_id)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.warehouse_id)
                    .collect::<Vec<_>>(),
            )
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.unit_price.unwrap_or_default())
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.charge_id)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.account_id)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.description.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.discount_percent)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.discount_amount)
                    .collect::<Vec<_>>(),
            )
            .bind(units.iter().map(|(uom, _)| uom.clone()).collect::<Vec<_>>())
            .bind(units.iter().map(|(_, factor)| *factor).collect::<Vec<_>>())
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The vendor and what the order comes to: each line's quantity
    /// times its price, rounded as an invoice for it would be.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let party: Option<(Uuid, String)> =
            sqlx::query_as("SELECT id, name FROM vendor WHERE id = $1")
                .bind(document::body::uuid(&body["vendor_id"]))
                .fetch_optional(&mut *conn)
                .await?;
        let currency = body["currency"].as_str().unwrap_or_default().to_owned();
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
        let _ = doc;
        Ok(json!({
            "vendor_id": party.as_ref().map(|(id, _)| id),
            "vendor_name": party.as_ref().map(|(_, name)| name),
            "currency": currency,
            "total_net": total,
            "total_discount": discount,
            "total_charges": charges,
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (vendor, currency, terms, billing): (Uuid, String, Option<Uuid>, Option<Uuid>) =
            sqlx::query_as(
                "SELECT o.vendor_id, COALESCE(o.currency, c.base_currency), o.payment_terms_id,
                    o.billing_address_id
             FROM purchase_order o
             JOIN company c ON c.id = $2 WHERE o.document_id = $1",
            )
            .bind(doc.id)
            .bind(doc.company_id)
            .fetch_one(&mut *conn)
            .await?;
        let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
        let mut lines = order_lines(conn, doc.id).await?;
        for line in &mut lines {
            line.net_amount = Some(line.net(decimals));
        }
        Ok(json!({
            "vendor_id": vendor,
            "currency": currency,
            "payment_terms_id": terms,
            "billing_address_id": billing,
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
        let spec = CommandSpec::action(self.name, self.description).permission(self.permission);
        match self.to {
            "open" => spec.from_states(&["draft"]),
            "cancelled" => spec.from_states(&["draft", "open"]).destructive(),
            _ => spec.from_states(&["open"]),
        }
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
            return Err(Error::state(format!(
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
                check_vendor(ctx, &order, PURCHASE_ORDER).await?;
                // A price or discount changed from what was proposed goes
                // to the company's policy.
                let prices = price_lines(&mut *ctx.conn, &lines).await?;
                erpcraft_pricing::check(
                    ctx,
                    order.company_id,
                    order.id,
                    erpcraft_pricing::Side::Purchase,
                    &prices,
                )
                .await?;
                // The parties the order is sent with.
                let (vendor, billing): (Uuid, Option<Uuid>) = sqlx::query_as(
                    "SELECT vendor_id, billing_address_id FROM purchase_order
                     WHERE document_id = $1",
                )
                .bind(order.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
                erpcraft_party::snapshot_purchase(
                    &mut *ctx.conn,
                    order.id,
                    order.company_id,
                    vendor,
                    billing,
                )
                .await?;
                ctx.decide(
                    policy::APPROVE_ORDER,
                    order.company_id,
                    policy::approve_order(&order.summary, lines.len()),
                )
                .await?;
                // Fail now rather than at the first receipt.
                settings(&mut *ctx.conn, order.company_id).await?;
            }
            "cancelled" => {
                if lines
                    .iter()
                    .any(|l| !(l.qty_received + l.qty_returned).is_zero())
                {
                    return Err(Error::state(
                        "goods have been received against this order; close it instead of cancelling it",
                    ));
                }
            }
            _ => {
                if lines.iter().any(|l| l.qty_invoiced != l.qty_received) {
                    return Err(Error::state(
                        "goods have been received that are not yet invoiced; the order must stay open for the invoice",
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
    /// Purchase order id.
    id: Uuid,
    /// Date of the new document. Today when omitted.
    doc_date: Option<NaiveDate>,
}

/// Drafts the document that follows from an open purchase order, with a line
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
        let order = document::lock(ctx, PURCHASE_ORDER, input.id, "read").await?;
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
        let rows: Vec<Value> = if self.doc_type == PURCHASE_RECEIPT {
            lines
                .iter()
                .filter(|l| l.qty_received < l.qty)
                .map(|l| json!({ "order_line_id": l.id, "qty": l.qty - l.qty_received }))
                .collect()
        } else {
            lines
                .iter()
                .filter(|l| l.qty_invoiced < l.qty_received)
                .map(|l| {
                    json!({
                        "order_line_id": l.id,
                        "qty": l.qty_received - l.qty_invoiced,
                        "unit_price": l.unit_price,
                    })
                })
                .collect()
        };
        if rows.is_empty() {
            return Err(Error::validation(if self.doc_type == PURCHASE_RECEIPT {
                "everything on this order has been received"
            } else {
                "nothing has been received on this order that is not yet invoiced"
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
                body: json!({ "purchase_order_id": order.id, "lines": rows }),
            },
        )
        .await
    }
}

// --------------------------------------------------------- purchase receipt

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    purchase_order_id: Uuid,
    /// For an order in a foreign currency: the rate to state. The rate on
    /// record for the receipt's date when omitted.
    fx_rate: Option<Decimal>,
    #[serde(default)]
    lines: Vec<ReceiptLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    order_line_id: Uuid,
    qty: Decimal,
    #[serde(default)]
    lots: Vec<LineLot>,
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
/// The currency a document that follows an order is in: the order's.
fn currency_field() -> FieldMeta {
    FieldMeta::new(
        "currency",
        "Currency",
        FieldType::link_by("currency", "code"),
    )
    .readonly()
    .help("The order's currency.")
}

/// The payment terms an order or invoice is on.
/// What open purchase orders of a company still have to bring in, per
/// item and warehouse, in stock units: each stock line's quantity less what
/// was received and kept (received less returned), times its factor.
pub async fn open_supply(
    conn: &mut PgConnection,
    company_id: Uuid,
) -> Result<Vec<(Uuid, Uuid, Decimal)>> {
    Ok(sqlx::query_as(
        "SELECT l.item_id, l.warehouse_id,
                sum(GREATEST(l.qty - COALESCE(p.qty_received - p.qty_returned, 0), 0) * l.factor)
         FROM purchase_order_line l
         JOIN document d ON d.id = l.document_id
         LEFT JOIN purchase_order_progress p ON p.line_id = l.id
         WHERE d.company_id = $1 AND d.status = 'open' AND l.item_id IS NOT NULL
         GROUP BY l.item_id, l.warehouse_id
         HAVING sum(GREATEST(l.qty - COALESCE(p.qty_received - p.qty_returned, 0), 0)) > 0",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The vendor's address a document names: where it sells from.
fn vendor_address_field() -> FieldMeta {
    erpcraft_party::address_field(
        "billing_address_id",
        "Vendor address",
        "vendor",
        "vendor_id",
        "billing",
        "One of the vendor's addresses; the order's, else its default billing address, when \
         left out. The document keeps a copy of it when it is posted.",
    )
}

/// The vendor of an invoice: its own, or its order's.
pub(crate) async fn invoice_vendor(conn: &mut PgConnection, invoice: Uuid) -> Result<Uuid> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(i.vendor_id, o.vendor_id)
         FROM vendor_invoice i LEFT JOIN purchase_order o ON o.document_id = i.purchase_order_id
         WHERE i.document_id = $1",
    )
    .bind(invoice)
    .fetch_one(&mut *conn)
    .await?)
}

/// Keep on `doc` the parties of the document it follows (`from`): a
/// receipt or a return those of its order, a credit note those of its
/// invoice. When that one kept none, the vendor's defaults now.
pub(crate) async fn keep_parties(
    conn: &mut PgConnection,
    doc: &Document,
    from: Uuid,
    vendor: Uuid,
) -> Result<()> {
    if !erpcraft_party::copy_parties(&mut *conn, from, doc.id).await? {
        erpcraft_party::snapshot_purchase(conn, doc.id, doc.company_id, vendor, None).await?;
    }
    Ok(())
}

fn terms_field(help: &str) -> FieldMeta {
    FieldMeta::new(
        "payment_terms_id",
        "Payment terms",
        FieldType::link("payment_terms"),
    )
    .help(help)
}

/// The exchange rate a document in a foreign currency states or posted at.
fn rate_field() -> FieldMeta {
    FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
        .rate(erpcraft_gl::RATE_PLACES)
        .help(
            "For an order in another currency: accounting currency per unit. The rate on record \
             for the document's date when left out; once posted, the rate it was posted at.",
        )
}

/// The currency of the order a receipt, invoice or credit note (`table`)
/// follows, and the rate the document states or, once posted, posted at.
async fn currency_of(
    conn: &mut PgConnection,
    table: &str,
    doc: &Document,
) -> Result<(String, Option<Decimal>)> {
    let found: Option<(String, Option<Decimal>)> = sqlx::query_as(&format!(
        "SELECT COALESCE(o.currency, {own}c.base_currency), t.fx_rate FROM {table} t
         LEFT JOIN purchase_order o ON o.document_id = t.purchase_order_id
         JOIN company c ON c.id = $2
         WHERE t.document_id = $1",
        // A direct vendor invoice is in a currency of its own.
        own = if table == "vendor_invoice" {
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

/// Keep the rate a draft states: checked when it posts, against the rate
/// on record, by the company's policy.
async fn state_rate(
    ctx: &mut Ctx<'_>,
    table: &str,
    doc: &Document,
    stated: Option<Decimal>,
) -> Result<()> {
    if stated.is_some_and(|rate| {
        rate <= Decimal::ZERO || rate.normalize().scale() > erpcraft_gl::RATE_PLACES
    }) {
        return Err(Error::validation(
            "an exchange rate is more than zero, with at most 18 decimal places",
        )
        .with_field("fx_rate"));
    }
    sqlx::query(&format!(
        "UPDATE {table} SET fx_rate = $2 WHERE document_id = $1"
    ))
    .bind(doc.id)
    .bind(stated)
    .execute(&mut *ctx.conn)
    .await?;
    Ok(())
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

/// The currency a summary shows amounts in, from the document's body, and
/// the conversion to work out tax in it (`None` in the accounting
/// currency; the rate is not used for what a summary shows).
async fn summary_currency(
    conn: &mut PgConnection,
    doc: &Document,
    body: &Value,
) -> Result<(String, Option<erpcraft_tax::Conversion>)> {
    let base = company::base_currency(conn, doc.company_id).await?;
    let currency = body["currency"]
        .as_str()
        .map_or(base.clone(), str::to_owned);
    let conversion = (currency != base).then(|| erpcraft_tax::Conversion {
        currency: currency.clone(),
        rate: document::body::decimal(&body["fx_rate"]).unwrap_or(Decimal::ONE),
    });
    Ok((currency, conversion))
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

/// What an amount in the order's currency is worth in the accounting
/// currency at `rate`, rounded to its decimals.
fn value_of(amount: Decimal, rate: Decimal, decimals: u32) -> Decimal {
    round_money(amount * rate, decimals)
}

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
                FieldMeta::new(
                    "purchase_order_id",
                    "Purchase order",
                    FieldType::document_in(PURCHASE_ORDER, &["open"]),
                )
                .required()
                .immutable(),
                currency_field(),
                rate_field(),
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
                        FieldType::line_of("purchase_order_id", PURCHASE_ORDER, "lines"),
                    )
                    .required(),
                    FieldMeta::new("qty", "Quantity received", FieldType::Decimal)
                        .quantity()
                        .required(),
                    FieldMeta::new("value", "Value", FieldType::Decimal)
                        .money()
                        .readonly(),
                    lots::field(),
                ],
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
            summary: vec![
                FieldMeta::new(
                    "purchase_order_id",
                    "Purchase order",
                    FieldType::document(PURCHASE_ORDER),
                )
                .readonly(),
                FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor")).readonly(),
                FieldMeta::new("vendor_name", "Vendor name", FieldType::Text)
                    .readonly()
                    .shown_from("vendor_id", "name"),
                FieldMeta::new("total_value", "Value", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once posted: what the goods came into stock for."),
            ],
            // 1: currency, accounting totals and their declarations.
            summary_version: 1,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: ReceiptBody =
            erpcraft_kernel::input::parse(body.clone(), "purchase receipt body")?;
        for (index, line) in body.lines.iter().enumerate() {
            if line.qty <= Decimal::ZERO {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("qty"),
                    "the quantity must be more than zero",
                ));
            }
            lots::check("lines", index + 1, &line.lots)?;
        }
        save_order_ref(
            ctx,
            "purchase_receipt",
            doc,
            body.purchase_order_id,
            "receipt",
        )
        .await?;
        state_rate(ctx, "purchase_receipt", doc, body.fx_rate).await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.purchase_order_id, &ids).await?;

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "purchase_receipt_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM purchase_receipt_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO purchase_receipt_line (id, document_id, line_no, order_line_id, qty)
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
        let named: Vec<(Uuid, &[LineLot])> = line_ids
            .iter()
            .zip(&body.lines)
            .map(|(id, line)| (*id, line.lots.as_slice()))
            .collect();
        lots::save(&mut *ctx.conn, doc.id, &named).await?;
        Ok(())
    }

    /// The order and its vendor, and once posted the value of the goods:
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
        let order: Uuid = sqlx::query_scalar(
            "SELECT purchase_order_id FROM purchase_receipt WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        let (currency, fx_rate) = currency_of(conn, "purchase_receipt", doc).await?;
        let mut body = json!({
            "purchase_order_id": order,
            "currency": currency,
            "fx_rate": fx_rate,
            "lines": receipt_lines(conn, doc.id).await?,
        });
        lots::attach(conn, doc.id, &mut body, "lines").await?;
        Ok(body)
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
             received not invoiced. An order can be received in several parts; more than was \
             ordered only where the company's policy allows it. Posting a receipt that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("purchase_receipt:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PURCHASE_RECEIPT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
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
        check_open(
            ctx,
            order.company_id,
            &order.status,
            order_lines.values(),
            PURCHASE_RECEIPT,
            "goods can be received only against an open order",
        )
        .await?;
        check_vendor(ctx, &order, PURCHASE_RECEIPT).await?;
        let lines = receipt_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a receipt needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        // Goods are valued in the accounting currency at the receipt's own
        // rate; that value stays with them in stock and in GRNI.
        let (currency, stated) = currency_of(&mut *ctx.conn, "purchase_receipt", &doc).await?;
        let rate = erpcraft_gl::document_rate(
            ctx,
            doc.company_id,
            &currency,
            doc.doc_date,
            stated,
            PURCHASE_RECEIPT,
        )
        .await?;
        let foreign = currency != company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let txn_decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?;
        let base_decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;

        let over_receipt = ctx.policy(policy::OVER_RECEIPT, doc.company_id).await?;
        let named = lots::of_document(&mut *ctx.conn, doc.id).await?;
        let mut movements = vec![];
        // The inventory account of each line, by what is received where.
        let mut inventory = vec![];
        for line in &lines {
            let ordered = order_lines
                .get(&line.order_line_id)
                .ok_or_else(|| Error::internal("receipt line without its order line"))?;
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
            let facts = policy::over_receipt(ordered.qty, ordered.qty_received, line.qty);
            if let Decision::Refused(refusal) = over_receipt.evaluate(facts)? {
                let open = (ordered.qty - ordered.qty_received).max(Decimal::ZERO);
                let why = refusal.message.unwrap_or_else(|| {
                    format!(
                        "{} are still to be received on order line {}, not {}",
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
            // At the order line's net price: its net amount for what was
            // ordered, in proportion to what came in.
            let discounted =
                ordered.discount_percent.is_some() || ordered.discount_amount.is_some();
            let cost = if foreign {
                // The line at the order price in its currency, then its
                // value at the receipt's rate.
                let txn = if discounted {
                    round_money(
                        line.qty * ordered.net(txn_decimals) / ordered.qty,
                        txn_decimals,
                    )
                } else {
                    round_money(line.qty * ordered.unit_price, txn_decimals)
                };
                Cost::Total(value_of(txn, rate, base_decimals))
            } else if discounted {
                Cost::Total(round_money(
                    line.qty * ordered.net(base_decimals) / ordered.qty,
                    base_decimals,
                ))
            } else if ordered.factor != Decimal::ONE {
                // The price is per the line's unit, the movement in stock
                // units: the value is what the line comes to.
                Cost::Total(round_money(line.qty * ordered.unit_price, base_decimals))
            } else {
                Cost::Unit(ordered.unit_price)
            };
            let stock_qty = ordered.in_stock_units(line.qty, line.line_no)?;
            let lots = lots::resolve(
                &mut *ctx.conn,
                doc.company_id,
                item_id,
                stock_qty,
                named.get(&line.id).map_or(&[][..], Vec::as_slice),
                lots::At {
                    collection: "lines",
                    line_no: line.line_no as usize,
                },
            )
            .await?;
            movements.push(StockLine {
                item_id,
                warehouse_id,
                qty: stock_qty,
                cost: Some(cost),
                source_line_id: Some(line.id),
                lots,
            });
        }

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(ctx, &posting, &movements, "purchase_receipt").await?;
        if foreign {
            sqlx::query("UPDATE purchase_receipt SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }

        let mut gl = vec![];
        let mut values = vec![];
        for ((line, entry), inventory) in lines.iter().zip(&entries).zip(inventory) {
            // What the goods cost is owed, whatever part of it became
            // stock value.
            let cost = entry.cost();
            // Replacements for goods returned and not credited can make
            // the line level again (as much kept as invoiced); GRNI then
            // takes what levels it, and any difference from the cost is
            // price variance.
            let mut owed = cost;
            if let Some(ordered) = order_lines.get_mut(&line.order_line_id) {
                if ordered.qty_received + line.qty == ordered.qty_invoiced {
                    owed = ordered.cleared_value - ordered.received_value;
                }
                ordered.qty_received += line.qty;
                ordered.received_value += owed;
            }
            values.push(Some(owed));
            let variance = owed - cost;
            if !variance.is_zero() {
                gl.push(gl_line(
                    accounts.price_variance_account_id,
                    variance.max(Decimal::ZERO),
                    (-variance).max(Decimal::ZERO),
                    line.id,
                ));
            }
            if !entry.value.is_zero() {
                gl.push(gl_line(inventory, entry.value, Decimal::ZERO, line.id));
            }
            // Goods that arrived on stock below zero: the part of their
            // cost that differs from what the missing goods left for.
            if !entry.variance.is_zero() {
                let account = erpcraft_gl::account_for(
                    &mut *ctx.conn,
                    doc.company_id,
                    "stock_variance",
                    &[("item", entry.item_id), ("warehouse", entry.warehouse_id)],
                )
                .await?;
                gl.push(gl_line(
                    account,
                    entry.variance.max(Decimal::ZERO),
                    (-entry.variance).max(Decimal::ZERO),
                    line.id,
                ));
            }
            if !owed.is_zero() {
                gl.push(gl_line(
                    accounts.grni_account_id,
                    Decimal::ZERO,
                    owed,
                    line.id,
                ));
            }
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "purchase_receipt_line",
            &ids,
            &[("value", &values)],
        )
        .await?;
        if !gl.is_empty() {
            let scopes = erpcraft_stock::line_scopes(&mut *ctx.conn, &entries).await?;
            let vendor = order_vendor(&mut *ctx.conn, order_id).await?;
            erpcraft_gl::add_scopes(&mut gl, &scopes, &[("vendor", vendor)]);
            erpcraft_gl::post(ctx, &posting, &gl).await?;
        }

        settle_order(ctx, &mut order, &order_lines, false).await?;
        // The parties as the order showed them, kept from now on.
        let vendor: Uuid =
            sqlx::query_scalar("SELECT vendor_id FROM purchase_order WHERE document_id = $1")
                .bind(order_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        crate::keep_parties(&mut *ctx.conn, &doc, order_id, vendor).await?;
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
        .from_states(&["posted"])
        .destructive()
        .permission("purchase_receipt:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PURCHASE_RECEIPT, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
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
                return Err(Error::state(format!(
                    "line {}: these goods have been invoiced; reverse the invoice first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
            ordered.qty_received -= line.qty;
            ordered.received_value -= line.value.unwrap_or_default();
            // An invoice clears the average value of everything received on
            // its line, so after a partial invoice the value left may not
            // be this receipt's own, to the cent.
            let left = ordered.received_value - ordered.cleared_value;
            let unbilled = ordered.qty_received - ordered.qty_invoiced;
            if left.is_sign_negative() || (unbilled.is_zero() && !left.is_zero()) {
                return Err(Error::state(format!(
                    "line {}: part of this order line has been invoiced at the average value of its receipts; reverse that invoice first",
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

// ----------------------------------------------------------- vendor invoice

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvoiceBody {
    /// The order invoiced; absent for a direct invoice (rent, a service),
    /// which names its vendor and has lines of its own only.
    purchase_order_id: Option<Uuid>,
    /// For a direct invoice: the vendor.
    vendor_id: Option<Uuid>,
    /// For a direct invoice: its currency, the accounting currency when
    /// omitted. An invoice of an order is in the order's.
    currency: Option<String>,
    vendor_reference: Option<String>,
    /// For an order in a foreign currency: the rate to state. The rate on
    /// record for the invoice's date when omitted.
    fx_rate: Option<Decimal>,
    /// The order's terms when omitted.
    payment_terms_id: Option<Uuid>,
    /// One of the vendor's addresses; the order's, else the vendor's
    /// default billing address, when omitted.
    billing_address_id: Option<Uuid>,
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
    /// For a landed-cost charge: `value` (the default) or `quantity`.
    allocate_by: Option<String>,
    /// For a landed-cost charge: the receipts whose goods it is a cost of;
    /// every receipt of the invoice's order when omitted.
    receipt_ids: Option<Vec<Uuid>>,
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
    cleared_value: Option<Decimal>,
    tax_code_id: Option<Uuid>,
    allocate_by: Option<String>,
    receipt_ids: Option<Vec<Uuid>>,
    /// Set when the invoice is posted: the net amount and the tax.
    net_amount: Option<Decimal>,
    tax_amount: Option<Decimal>,
}

async fn invoice_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<InvoiceLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, charge_id, account_id, description, qty, unit_price,
                discount_percent, discount_amount, cleared_value, tax_code_id, allocate_by,
                receipt_ids, net_amount, tax_amount
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
                FieldMeta::new(
                    "purchase_order_id",
                    "Purchase order",
                    FieldType::document_in(PURCHASE_ORDER, &["open"]),
                )
                .immutable()
                .help("Absent for an invoice without an order (rent, a service): it names its vendor."),
                FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                    .help("For an invoice without an order; one of an order is with the order's vendor."),
                FieldMeta::new(
                    "vendor_reference",
                    "Vendor's invoice number",
                    FieldType::Text,
                ),
                FieldMeta::new("currency", "Currency", FieldType::link_by("currency", "code")).help(
                    "The order's currency; for an invoice without an order, its own, the \
                     accounting currency when left out.",
                ),
                rate_field(),
                terms_field(
                    "The order's terms when left out. They set when the invoice is due and the \
                     discount for paying it early.",
                ),
                vendor_address_field(),
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
                        FieldType::line_of("purchase_order_id", PURCHASE_ORDER, "lines"),
                    )
                    .help("The order line invoiced; absent for a charge or an account line."),
                ]
                .into_iter()
                .chain(erpcraft_tax::lines::line_kind_fields(
                    "For a line that is not stock and not a charge: the expense or asset account.",
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
                    FieldMeta::new(
                        "allocate_by",
                        "Allocate by",
                        FieldType::options(&["value", "quantity", "weight"]),
                    )
                    .help("For a landed-cost charge: shared out over the goods by their value (the default) or their quantity."),
                    FieldMeta::new("receipt_ids", "Receipts", FieldType::Json).help(
                        "For a landed-cost charge: the receipts whose goods it is a cost of, by id; \
                         every receipt of the order when left out.",
                    ),
                    FieldMeta::new("net_amount", "Net", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                    // Both are set when the invoice is posted.
                    FieldMeta::new("tax_amount", "Tax", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                    FieldMeta::new(
                        "cleared_value",
                        "Value of the goods invoiced",
                        FieldType::Decimal,
                    )
                    .money()
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
                    "purchase_order_id",
                    "Purchase order",
                    FieldType::document(PURCHASE_ORDER),
                )
                .readonly(),
                FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor")).readonly(),
                FieldMeta::new("vendor_name", "Vendor name", FieldType::Text)
                    .readonly()
                    .shown_from("vendor_id", "name"),
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
            erpcraft_kernel::input::parse(body.clone(), "vendor invoice body")?;
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
            if line.charge_id.is_none()
                && (line.allocate_by.is_some() || line.receipt_ids.is_some())
            {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("allocate_by"),
                    "only a charge is allocated to goods received",
                ));
            }
            if line
                .allocate_by
                .as_deref()
                .is_some_and(|by| !["value", "quantity", "weight"].contains(&by))
            {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("allocate_by"),
                    "a charge is allocated by value, by quantity or by weight",
                ));
            }
        }
        match body.purchase_order_id {
            Some(order_id) => {
                save_order_ref(ctx, "vendor_invoice", doc, order_id, "invoice").await?;
            }
            None => save_direct(ctx, doc, &body).await?,
        }
        state_rate(ctx, "vendor_invoice", doc, body.fx_rate).await?;
        let reference = body
            .vendor_reference
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        if let Some(address) = body.billing_address_id {
            let vendor = invoice_vendor(&mut *ctx.conn, doc.id).await?;
            erpcraft_party::check_address(
                &mut *ctx.conn,
                "vendor",
                vendor,
                address,
                "billing_address_id",
            )
            .await?;
        }
        // The order's terms and address unless the invoice names others.
        sqlx::query(
            "UPDATE vendor_invoice i SET vendor_reference = $2,
                payment_terms_id = COALESCE($3, (SELECT o.payment_terms_id FROM purchase_order o
                                                 WHERE o.document_id = i.purchase_order_id),
                                            (SELECT v.payment_terms_id FROM vendor v
                                             WHERE v.id = i.vendor_id)),
                billing_address_id = COALESCE($4, (SELECT o.billing_address_id
                                                   FROM purchase_order o
                                                   WHERE o.document_id = i.purchase_order_id))
             WHERE i.document_id = $1",
        )
        .bind(doc.id)
        .bind(reference)
        .bind(body.payment_terms_id)
        .bind(body.billing_address_id)
        .execute(&mut *ctx.conn)
        .await?;
        let ids: Vec<Uuid> = body.lines.iter().filter_map(|l| l.order_line_id).collect();
        if let Some(order_id) = body.purchase_order_id {
            check_order_lines(ctx, order_id, &ids).await?;
        }
        // A line of an order takes the order line's discounts unless it
        // gives its own, the amount in proportion to the quantity.
        let party = invoice_vendor(&mut *ctx.conn, doc.id).await?;
        let ordered: BTreeMap<Uuid, OrderLine> = match body.purchase_order_id {
            Some(order_id) => order_lines(&mut *ctx.conn, order_id)
                .await?
                .into_iter()
                .map(|l| (l.id, l))
                .collect(),
            None => BTreeMap::new(),
        };
        let (currency, _) = currency_of(&mut *ctx.conn, "vendor_invoice", doc).await?;
        let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?;
        // Tax by destination on a purchase: where the company receives the
        // goods, and whether the vendor is within each tax's jurisdiction.
        let place = erpcraft_party::ship_to(&mut *ctx.conn, "company", doc.company_id, None, None)
            .await?
            .map(|a| erpcraft_tax::Place {
                country_code: a.country_code,
                region: a.region,
                postcode: a.postcode,
                city: a.city,
            });
        let vendor_at = erpcraft_party::load(&mut *ctx.conn, "vendor", party)
            .await?
            .address
            .map(|a| (a.country_code, a.region));
        for line in &mut body.lines {
            if let Some(order_line) = line.order_line_id.and_then(|id| ordered.get(&id))
                && line.discount_percent.is_none()
                && line.discount_amount.is_none()
            {
                line.discount_percent = order_line.discount_percent;
                line.discount_amount = order_line
                    .discount_amount
                    .map(|a| round_money(a * line.qty / order_line.qty, decimals));
            }
            line.discount_percent = erpcraft_tax::lines::no_zero(line.discount_percent);
            line.discount_amount = erpcraft_tax::lines::no_zero(line.discount_amount);
            // No code given: what the tax rules propose for the party, the
            // item or the charge.
            if line.tax_code_id.is_none() {
                let ordered_line = line.order_line_id.and_then(|id| ordered.get(&id));
                line.tax_code_id = erpcraft_tax::propose(
                    &mut *ctx.conn,
                    &erpcraft_tax::Proposal {
                        company_id: doc.company_id,
                        date: doc.doc_date,
                        side: erpcraft_tax::Side::Purchase {
                            vendor_id: party,
                            vendor_at: vendor_at.clone(),
                        },
                        item_id: ordered_line.and_then(|l| l.item_id),
                        charge_id: line.charge_id.or(ordered_line.and_then(|l| l.charge_id)),
                        place: place.as_ref(),
                    },
                )
                .await?;
            }
        }

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "vendor_invoice_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM vendor_invoice_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO vendor_invoice_line (id, document_id, line_no, order_line_id, qty,
                    unit_price, tax_code_id, charge_id, account_id, description, discount_percent,
                    discount_amount, allocate_by, receipt_ids)
                 SELECT r.id, $1, r.line_no, r.order_line_id, r.qty, r.unit_price, r.tax_code_id,
                    r.charge_id, r.account_id, NULLIF(trim(r.description), ''),
                    r.discount_percent, r.discount_amount, r.allocate_by,
                    CASE WHEN r.receipts = '' THEN NULL
                         ELSE string_to_array(r.receipts, ',')::uuid[] END
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[], $6::numeric[],
                             $7::uuid[], $8::uuid[], $9::uuid[], $10::text[], $11::numeric[],
                             $12::numeric[], $13::text[], $14::text[])
                    AS r (id, line_no, order_line_id, qty, unit_price, tax_code_id, charge_id,
                          account_id, description, discount_percent, discount_amount,
                          allocate_by, receipts)",
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
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.unit_price)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.tax_code_id)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.charge_id)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.account_id)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.description.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.discount_percent)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.discount_amount)
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| line.allocate_by.clone())
                    .collect::<Vec<_>>(),
            )
            .bind(
                body.lines
                    .iter()
                    .map(|line| {
                        line.receipt_ids.as_ref().map_or(String::new(), |ids| {
                            ids.iter()
                                .map(Uuid::to_string)
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The order and its vendor, and what the invoice comes to: net, tax
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
        let (currency, conversion) = summary_currency(conn, doc, body).await?;
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
                    Direction::Paid,
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
            "SELECT id FROM purchase_order_line WHERE id = ANY($1) AND charge_id IS NOT NULL",
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
        #[allow(clippy::type_complexity)]
        let (order, vendor, reference, terms, billing): (
            Option<Uuid>,
            Option<Uuid>,
            Option<String>,
            Option<Uuid>,
            Option<Uuid>,
        ) = sqlx::query_as(
            "SELECT purchase_order_id, vendor_id, vendor_reference, payment_terms_id,
                    billing_address_id
             FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        let (currency, fx_rate) = currency_of(conn, "vendor_invoice", doc).await?;
        let mut body = json!({
            "purchase_order_id": order,
            "vendor_id": vendor,
            "vendor_reference": reference,
            "payment_terms_id": terms,
            "billing_address_id": billing,
            "currency": currency,
            "fx_rate": fx_rate,
            "lines": invoice_lines(conn, doc.id).await?,
        });
        fill_draft_lines(conn, doc, &mut body, Some(Direction::Paid)).await?;
        Ok(body)
    }
}

/// The account a purchase line that is not stock is a cost of: its
/// account, or its charge's (`charge_expense` for the charge, else the
/// company's default for `charge_expense`).
pub(crate) async fn expense_account(
    conn: &mut PgConnection,
    company_id: Uuid,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
) -> Result<Uuid> {
    match (account_id, charge_id) {
        (Some(account), _) => Ok(account),
        (None, Some(charge)) => {
            erpcraft_gl::account_for(conn, company_id, "charge_expense", &[("charge", charge)])
                .await
        }
        (None, None) => Err(Error::internal(
            "a line that is not stock without its account",
        )),
    }
}

/// Share a landed-cost charge of `amount` (in the accounting currency) out
/// over the goods of the receipts the line names, or of every posted
/// receipt of the order: by their value or by their quantity, rounded per
/// receipt line, the last taking what is left. Of what each item in each
/// warehouse gets, the share of its goods still on hand raises their value
/// (a cost adjustment in the stock ledger, debited to the inventory
/// account); the share of goods already gone, sold or used, is a cost of
/// those goods, debited to the account for `landed_cost_variance` (by item,
/// else the company's) or else purchase price variance, where purchasing
/// puts every other difference in what goods cost. Under average costing
/// the goods still on hand are taken to be the received ones as far as the
/// level holds that many.
async fn landed_cost(
    ctx: &mut Ctx<'_>,
    doc: &Document,
    order_id: Option<Uuid>,
    line: &InvoiceLine,
    amount: Decimal,
    price_variance: Uuid,
) -> Result<(Vec<GlLine>, Vec<erpcraft_stock::Adjustment>)> {
    let receipts: Vec<Uuid> = match &line.receipt_ids {
        Some(ids) => ids.clone(),
        None => match order_id {
            Some(order_id) => {
                sqlx::query_scalar(
                    "SELECT document_id FROM purchase_receipt WHERE purchase_order_id = $1",
                )
                .bind(order_id)
                .fetch_all(&mut *ctx.conn)
                .await?
            }
            None => vec![],
        },
    };
    #[allow(clippy::type_complexity)]
    let received: Vec<(Decimal, Decimal, Uuid, Uuid, Option<Decimal>, String)> = sqlx::query_as(
        "SELECT rl.qty * ol.factor, COALESCE(rl.value, 0), ol.item_id, ol.warehouse_id, it.weight,
                it.code
         FROM purchase_receipt_line rl
         JOIN document d ON d.id = rl.document_id
         JOIN purchase_order_line ol ON ol.id = rl.order_line_id
         JOIN item it ON it.id = ol.item_id
         WHERE rl.document_id = ANY($1) AND d.status = 'posted' AND d.company_id = $2
         ORDER BY d.doc_date, d.id, rl.line_no",
    )
    .bind(&receipts)
    .bind(doc.company_id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let refuse = |why: &str| {
        Error::line(
            "lines",
            line.line_no as usize,
            Some("receipt_ids"),
            why.to_owned(),
        )
    };
    if received.is_empty() {
        return Err(refuse(
            "no posted receipt brings in goods this charge could be a cost of",
        ));
    }
    let by = line.allocate_by.as_deref().unwrap_or("value");
    if by == "weight"
        && let Some(row) = received.iter().find(|r| r.4.is_none())
    {
        return Err(refuse(&format!(
            "item {} has no weight: give its items a weight, or allocate by value or quantity",
            row.5
        )));
    }
    let basis = |r: &(Decimal, Decimal, Uuid, Uuid, Option<Decimal>, String)| match by {
        "quantity" => r.0,
        "weight" => r.0 * r.4.unwrap_or_default(),
        _ => r.1,
    };
    let total: Decimal = received.iter().map(basis).sum();
    if total <= Decimal::ZERO {
        return Err(refuse(
            "the goods received have no value to share the charge out by; allocate it by quantity",
        ));
    }
    let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
    // Per item and warehouse: what it gets, and how much was received.
    let mut levels: Vec<((Uuid, Uuid), Decimal, Decimal)> = vec![];
    let mut left = amount;
    for (index, row) in received.iter().enumerate() {
        let share = if index + 1 == received.len() {
            left
        } else {
            round_money(amount * basis(row) / total, decimals).min(left)
        };
        left -= share;
        let key = (row.2, row.3);
        match levels.iter_mut().find(|(k, ..)| *k == key) {
            Some((_, got, qty)) => {
                *got += share;
                *qty += row.0;
            }
            None => levels.push((key, share, row.0)),
        }
    }
    erpcraft_stock::lock_levels(
        &mut *ctx.conn,
        doc.company_id,
        levels.iter().map(|(key, ..)| *key).collect(),
    )
    .await?;
    let mut gl = vec![];
    let mut adjustments = vec![];
    let places =
        erpcraft_stock::locations(&mut *ctx.conn, levels.iter().map(|((_, w), _, _)| *w)).await?;
    for ((item, warehouse), got, qty) in levels {
        // The goods it raises are what each of these lines is about.
        let scopes = places.scopes(warehouse, Some(item));
        let (on_hand, _) = erpcraft_stock::on_hand(&mut *ctx.conn, item, warehouse).await?;
        let still = on_hand.max(Decimal::ZERO).min(qty);
        let to_stock = if still == qty {
            got
        } else {
            round_money(got * still / qty, decimals)
        };
        let gone = got - to_stock;
        if !to_stock.is_zero() {
            let inventory = erpcraft_gl::account_for(
                &mut *ctx.conn,
                doc.company_id,
                "inventory",
                &[("item", item), ("warehouse", warehouse)],
            )
            .await?;
            gl.push(gl_line(inventory, to_stock, Decimal::ZERO, line.id).about(&scopes));
            adjustments.push(erpcraft_stock::Adjustment {
                item_id: item,
                warehouse_id: warehouse,
                value: to_stock,
                source_line_id: Some(line.id),
            });
        }
        if !gone.is_zero() {
            let account = erpcraft_gl::account_for(
                &mut *ctx.conn,
                doc.company_id,
                "landed_cost_variance",
                &[("item", item)],
            )
            .await
            .unwrap_or(price_variance);
            gl.push(gl_line(account, gone, Decimal::ZERO, line.id).about(&scopes));
        }
    }
    Ok((gl, adjustments))
}

/// Store the header of a direct invoice: its vendor and currency. Its
/// lines are its own: none may follow an order line.
async fn save_direct(ctx: &mut Ctx<'_>, doc: &Document, body: &InvoiceBody) -> Result<()> {
    let vendor = body.vendor_id.ok_or_else(|| {
        Error::validation("an invoice without a purchase order names its vendor")
            .with_field("vendor_id")
    })?;
    if let Some(index) = body.lines.iter().position(|l| l.order_line_id.is_some()) {
        return Err(Error::line(
            "lines",
            index + 1,
            Some("order_line_id"),
            "an invoice without a purchase order has no order lines: give a charge or an account",
        ));
    }
    let existing: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT purchase_order_id FROM vendor_invoice WHERE document_id = $1")
            .bind(doc.id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
    if existing.flatten().is_some() {
        return Err(Error::validation(
            "the purchase order of a document cannot change; create a new document instead",
        ));
    }
    let known: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM vendor WHERE id = $1)")
        .bind(vendor)
        .fetch_one(&mut *ctx.conn)
        .await?;
    if !known {
        return Err(Error::validation("the vendor does not exist").with_field("vendor_id"));
    }
    let base = company::base_currency(&mut *ctx.conn, doc.company_id).await?;
    let currency = body.currency.clone().filter(|c| *c != base);
    if let Some(currency) = &currency {
        erpcraft_gl::currency_decimals(&mut *ctx.conn, currency)
            .await
            .map_err(|e| e.with_field("currency"))?;
    }
    sqlx::query(
        "INSERT INTO vendor_invoice (document_id, vendor_id, currency) VALUES ($1, $2, $3)
         ON CONFLICT (document_id) DO UPDATE SET vendor_id = EXCLUDED.vendor_id,
            currency = EXCLUDED.currency",
    )
    .bind(doc.id)
    .bind(vendor)
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
            "vendor_invoice.post",
            "Post a draft vendor invoice against a purchase order. Only goods already received \
             can be invoiced. Payables are credited with what the vendor charges, goods received \
             not invoiced is debited with what those goods were received for, and any difference \
             goes to purchase price variance. Posting an invoice that is already posted returns \
             it unchanged.",
        )
        .from_states(&["draft"])
        .permission("vendor_invoice:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, VENDOR_INVOICE, input.id, "post").await?;
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
        let order_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT purchase_order_id FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        // An invoice of an order, or a direct one (rent, a service) with
        // lines of its own only.
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
                VENDOR_INVOICE,
                "only an open order can be invoiced",
            )
            .await?;
            check_vendor(ctx, order, VENDOR_INVOICE).await?;
        }
        let lines = invoice_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("an invoice needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        // Charged in the order's currency at the invoice's rate; GRNI is
        // cleared in the accounting currency by what the goods were
        // received for, and the difference, of price and of rate alike, is
        // purchase price variance.
        let (currency, stated) = currency_of(&mut *ctx.conn, "vendor_invoice", &doc).await?;
        let rate = erpcraft_gl::document_rate(
            ctx,
            doc.company_id,
            &currency,
            doc.doc_date,
            stated,
            VENDOR_INVOICE,
        )
        .await?;
        let base_decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?;
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
        let mut cleared_values = vec![];
        let mut net_amounts = vec![];
        let mut taxes = vec![];
        let mut gl = vec![];
        let mut adjustments = vec![];
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
            let charged_value = value_of(charged, rate, base_decimals);
            // What the line is a cost of: goods received (clearing GRNI),
            // an expense or asset account, or for a landed-cost charge the
            // goods of receipts.
            let mut cleared = None;
            match line.order_line_id {
                Some(order_line_id) => {
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
                            Some("qty"),
                            why,
                        ));
                    }
                    let open_qty = ordered.qty_received - ordered.qty_invoiced;
                    if line.qty > open_qty {
                        return Err(Error::line(
                            "lines",
                            line.line_no as usize,
                            Some("qty"),
                            format_args!(
                                "{} have been received and not yet invoiced on order line {}, not {}",
                                trim(open_qty),
                                ordered.line_no,
                                trim(line.qty)
                            ),
                        ));
                    }
                    ordered.qty_invoiced += line.qty;
                    if ordered.item_id.is_some() {
                        // What these goods were received for. The last of
                        // them takes the last of the value, so nothing is
                        // left in GRNI.
                        let open_value = ordered.received_value - ordered.cleared_value;
                        let value = if line.qty == open_qty {
                            open_value
                        } else {
                            round_money(line.qty * open_value / open_qty, base_decimals)
                                .min(open_value)
                        };
                        ordered.cleared_value += value;
                        cleared = Some(value);
                        if !value.is_zero() {
                            gl.push(gl_line(
                                accounts.grni_account_id,
                                value,
                                Decimal::ZERO,
                                line.id,
                            ));
                        }
                        // The vendor charged more than the goods were
                        // received for: a cost. Less: a gain.
                        let variance = charged_value - value;
                        if !variance.is_zero() {
                            gl.push(gl_line(
                                accounts.price_variance_account_id,
                                variance.max(Decimal::ZERO),
                                (-variance).max(Decimal::ZERO),
                                line.id,
                            ));
                        }
                    } else {
                        let account = expense_account(
                            &mut *ctx.conn,
                            doc.company_id,
                            ordered.charge_id,
                            ordered.account_id,
                        )
                        .await?;
                        if !charged_value.is_zero() {
                            gl.push(gl_line(account, charged_value, Decimal::ZERO, line.id));
                        }
                    }
                }
                None => {
                    let landed: bool = match line.charge_id {
                        Some(charge) => {
                            sqlx::query_scalar("SELECT landed_cost FROM charge WHERE id = $1")
                                .bind(charge)
                                .fetch_one(&mut *ctx.conn)
                                .await?
                        }
                        None => false,
                    };
                    if landed {
                        let (lines, adjusted) = landed_cost(
                            ctx,
                            &doc,
                            order_id,
                            line,
                            charged_value,
                            accounts.price_variance_account_id,
                        )
                        .await?;
                        gl.extend(lines);
                        adjustments.extend(adjusted);
                    } else {
                        let account = expense_account(
                            &mut *ctx.conn,
                            doc.company_id,
                            line.charge_id,
                            line.account_id,
                        )
                        .await?;
                        if !charged_value.is_zero() {
                            gl.push(gl_line(account, charged_value, Decimal::ZERO, line.id));
                        }
                    }
                }
            }
            net_amounts.push(Some(charged));
            cleared_values.push(cleared);
            owed += charged;
            owed_value += charged_value;
            // Tax comes on top of the net amount and is owed with it.
            if let Some(code) = line.tax_code_id {
                // One tax per component of a group, each to its own account.
                let line_taxes = erpcraft_tax::quote(
                    ctx,
                    doc.company_id,
                    code,
                    Direction::Paid,
                    charged,
                    Some(line.id),
                    conversion.as_ref(),
                )
                .await?;
                tax_amounts.push(Some(erpcraft_tax::Tax::total(&line_taxes)));
                for tax in line_taxes {
                    if tax.self_assessed {
                        // Owed by the company itself and reclaimed: on the
                        // tax accounts alone, not owed to the vendor.
                        if !tax.value().is_zero() {
                            let (debit, credit) = tax.debit_credit();
                            gl.push(gl_line(tax.account_id, debit, credit, line.id));
                        }
                    } else if !tax.amount.is_zero() {
                        owed += tax.amount;
                        owed_value += tax.value();
                        // The tax account holds the accounting value; what
                        // is owed for it is in the invoice's currency.
                        gl.push(gl_line(tax.account_id, tax.value(), Decimal::ZERO, line.id));
                        gl.push(in_currency(
                            gl_line(
                                accounts.payable_account_id,
                                Decimal::ZERO,
                                tax.amount,
                                line.id,
                            ),
                            conversion.as_ref(),
                        ));
                    }
                    taxes.push(tax);
                }
            } else {
                tax_amounts.push(None);
            }
            if !charged.is_zero() {
                gl.push(in_currency(
                    gl_line(accounts.payable_account_id, Decimal::ZERO, charged, line.id),
                    conversion.as_ref(),
                ));
            }
        }

        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "vendor_invoice_line",
            &ids,
            &[
                ("tax_amount", &tax_amounts),
                ("cleared_value", &cleared_values),
                ("net_amount", &net_amounts),
            ],
        )
        .await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        if !gl.is_empty() {
            let about: Vec<_> = lines
                .iter()
                .map(
                    |line| match line.order_line_id.and_then(|id| order_lines.get(&id)) {
                        Some(o) => (line.id, o.item_id, o.warehouse_id, o.charge_id),
                        None => (line.id, None, None, line.charge_id),
                    },
                )
                .collect();
            let scopes = gl_scopes(&mut *ctx.conn, &about).await?;
            let vendor = invoice_vendor(&mut *ctx.conn, doc.id).await?;
            erpcraft_gl::add_scopes(&mut gl, &scopes, &[("vendor", vendor)]);
            erpcraft_gl::post(ctx, &posting, &gl).await?;
        }
        erpcraft_stock::adjust(&mut *ctx.conn, &posting, &adjustments).await?;
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        // What is now owed, as one open item for a payment to settle.
        // No approved order stands behind a direct invoice: the company's
        // policy says whether it may be posted.
        if order.is_none() {
            let vendor_code: String = sqlx::query_scalar(
                "SELECT v.code FROM vendor_invoice i JOIN vendor v ON v.id = i.vendor_id
                 WHERE i.document_id = $1",
            )
            .bind(doc.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            ctx.decide(
                policy::WITHOUT_ORDER,
                doc.company_id,
                policy::without_order(owed, &currency, &vendor_code),
            )
            .await?;
        }
        if !owed.is_zero() {
            let (party_id, terms): (Uuid, Option<Uuid>) = sqlx::query_as(
                "SELECT COALESCE(i.vendor_id, o.vendor_id), i.payment_terms_id
                 FROM vendor_invoice i
                 LEFT JOIN purchase_order o ON o.document_id = i.purchase_order_id
                 WHERE i.document_id = $1",
            )
            .bind(doc.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            erpcraft_openitems::open(
                &mut *ctx.conn,
                &posting,
                &NewOpenItem {
                    kind: Kind::Payable,
                    party_type: "vendor",
                    party_id,
                    account_id: accounts.payable_account_id,
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
            sqlx::query("UPDATE vendor_invoice SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }
        if let Some(order) = &mut order {
            settle_order(ctx, order, &order_lines, false).await?;
        }
        // The parties as the invoice shows them, kept from now on.
        let billing: Option<Uuid> = sqlx::query_scalar(
            "SELECT billing_address_id FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let vendor = invoice_vendor(&mut *ctx.conn, doc.id).await?;
        erpcraft_party::snapshot_purchase(&mut *ctx.conn, doc.id, doc.company_id, vendor, billing)
            .await?;
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
        .from_states(&["posted"])
        .destructive()
        .permission("vendor_invoice:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, VENDOR_INVOICE, input.id, "reverse").await?;
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
        let order_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT purchase_order_id FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
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
                    "line {}: goods on this order line have been returned and credited since; reverse the credit note first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
            ordered.qty_invoiced -= line.qty;
            ordered.cleared_value -= line.cleared_value.unwrap_or_default();
            check_still_level(ordered, line.line_no)?;
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

/// A draft's line figures as posting will work them out, for its view and
/// a preview: each line's net amount where it has none yet, and with
/// `direction` the tax its code puts on it. A posted document keeps what
/// it posted.
pub(crate) async fn fill_draft_lines(
    conn: &mut PgConnection,
    doc: &Document,
    body: &mut Value,
    direction: Option<Direction>,
) -> Result<()> {
    use document::body::{decimal, uuid};
    if doc.status != "draft" {
        return Ok(());
    }
    let base = company::base_currency(conn, doc.company_id).await?;
    let currency = body["currency"]
        .as_str()
        .map_or(base.clone(), str::to_owned);
    let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
    let conversion = (currency != base).then(|| erpcraft_tax::Conversion {
        currency: currency.clone(),
        rate: decimal(&body["fx_rate"]).unwrap_or(Decimal::ONE),
    });
    let Some(lines) = body["lines"].as_array_mut() else {
        return Ok(());
    };
    for line in lines {
        let net = match decimal(&line["net_amount"]) {
            Some(net) => net,
            None => {
                let net = erpcraft_tax::lines::body_line_net(line, decimals);
                line["net_amount"] = json!(net);
                net
            }
        };
        if let Some(direction) = direction
            && line["tax_amount"].is_null()
            && let Some(code) = uuid(&line["tax_code_id"])
            && let Ok(taxes) = erpcraft_tax::estimate(
                &mut *conn,
                doc.company_id,
                code,
                direction,
                net,
                uuid(&line["id"]),
                conversion.as_ref(),
            )
            .await
        {
            line["tax_amount"] = json!(erpcraft_tax::Tax::total(&taxes));
        }
    }
    Ok(())
}
