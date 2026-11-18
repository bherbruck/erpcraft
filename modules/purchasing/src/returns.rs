//! Goods back to the vendor, and the vendor's credit for them.
//!
//! A **purchase return** sends goods received against an order back. They
//! leave stock at the moving average, as every issue does, and take out of
//! goods received not invoiced (GRNI) what they were received for: the
//! average received value of the order line. The two differ only where
//! the item's cost moved since; the difference goes to purchase price
//! variance, as an invoice's difference from the receipt does.
//!
//! A **vendor credit note** reduces what is owed for a posted invoice. The
//! part of a line covered by goods returned and not yet credited puts back
//! into GRNI what those goods were invoiced at; any other part is a credit
//! of price only, refused unless the company allows it
//! (`vendor_credit_note.without_return`). The credit is an open item below
//! zero, which a payment settles with the vendor's invoices, or
//! `payment.apply_credit` applies to one of them. Tax is taken back at the
//! invoice's code and rate, in the credit note's own period.
//!
//! Each step leaves the order line's GRNI exact: whichever document makes
//! net received and net invoiced level takes the value that is left.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, Error, FieldMeta,
    FieldType, JsonSchema, PgConnection, Result, StateMeta, async_trait, company, round_money,
    schemars,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use erpcraft_stock::StockLine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    DocInput, OrderLine, PURCHASE_ORDER, ReverseInput, VENDOR_INVOICE, check_order_lines,
    check_still_level, follows, gl_line, in_currency, lock_order, order_lines, policy,
    post_and_reverse, settings, settle_order, states, trim,
};

pub(crate) const PURCHASE_RETURN: &str = "purchase_return";
pub(crate) const VENDOR_CREDIT_NOTE: &str = "vendor_credit_note";

/// What returning `qty` of an order line takes out of GRNI: the average
/// received value of what is kept, except where the return leaves the line
/// level (net received equal to net invoiced) or empty; it then takes what
/// is left, so that nothing stays behind.
fn returned_value(line: &OrderLine, qty: Decimal, decimals: u32) -> Decimal {
    if qty == line.qty_received {
        line.received_value
    } else if line.qty_received - qty == line.qty_invoiced {
        line.received_value - line.cleared_value
    } else {
        round_money(qty * line.received_value / line.qty_received, decimals)
            .min(line.received_value)
    }
}

/// What crediting `qty` returned goods of an order line puts back into
/// GRNI: the average value invoiced goods cleared, except where the credit
/// leaves the line level or empty; it then takes what is left.
fn credited_value(line: &OrderLine, qty: Decimal, decimals: u32) -> Decimal {
    if qty.is_zero() {
        Decimal::ZERO
    } else if qty == line.qty_invoiced {
        line.cleared_value
    } else if line.qty_invoiced - qty == line.qty_received {
        line.cleared_value - line.received_value
    } else {
        round_money(qty * line.cleared_value / line.qty_invoiced, decimals).min(line.cleared_value)
    }
}

/// The order a return or credit note refers to: one of the document's
/// company that goods were received against (open or closed).
async fn check_returnable_order(ctx: &mut Ctx<'_>, doc: &Document, order_id: Uuid) -> Result<()> {
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
        Some("open" | "closed") => Ok(()),
        Some(status) => Err(Error::state(format!(
            "the purchase order is {status}; goods are returned only against an open or closed order"
        ))),
    }
}

// ---------------------------------------------------------- purchase return

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReturnBody {
    purchase_order_id: Uuid,
    #[serde(default)]
    lines: Vec<ReturnLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReturnLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    order_line_id: Uuid,
    qty: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct ReturnLine {
    id: Uuid,
    line_no: i32,
    order_line_id: Uuid,
    qty: Decimal,
    value: Option<Decimal>,
    stock_value: Option<Decimal>,
}

async fn return_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<ReturnLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, qty, value, stock_value FROM purchase_return_line
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

async fn return_order(conn: &mut PgConnection, document_id: Uuid) -> Result<Uuid> {
    let order =
        sqlx::query_scalar("SELECT purchase_order_id FROM purchase_return WHERE document_id = $1")
            .bind(document_id)
            .fetch_one(&mut *conn)
            .await?;
    Ok(order)
}

pub(crate) struct PurchaseReturn;

#[async_trait]
impl DocumentHandler for PurchaseReturn {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: PURCHASE_RETURN.into(),
            module: String::new(),
            label: "Purchase return".into(),
            number_prefix: "PR".into(),
            fields: vec![
                FieldMeta::new(
                    "purchase_order_id",
                    "Purchase order",
                    FieldType::document_in(PURCHASE_ORDER, &["open", "closed"]),
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
                        FieldType::line_of("purchase_order_id", PURCHASE_ORDER, "lines"),
                    )
                    .required(),
                    FieldMeta::new("qty", "Quantity returned", FieldType::Decimal)
                        .quantity()
                        .required(),
                    FieldMeta::new("value", "Value", FieldType::Decimal)
                        .money()
                        .readonly()
                        .help("Set when posted: what the goods were received for."),
                    FieldMeta::new("stock_value", "Stock value", FieldType::Decimal)
                        .money()
                        .readonly()
                        .help("Set when posted: what the goods left stock at."),
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
                    .help("Known once posted: what the goods were received for."),
            ],
            // 1: currency, accounting totals and their declarations.
            summary_version: 1,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: ReturnBody = erpcraft_kernel::input::parse(body.clone(), "purchase return body")?;
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
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT purchase_order_id FROM purchase_return WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        match existing {
            Some(current) if current != body.purchase_order_id => {
                return Err(Error::validation(
                    "the purchase order of a document cannot change; create a new document instead",
                ));
            }
            Some(_) => {}
            None => {
                check_returnable_order(ctx, doc, body.purchase_order_id).await?;
                sqlx::query(
                    "INSERT INTO purchase_return (document_id, purchase_order_id) VALUES ($1, $2)",
                )
                .bind(doc.id)
                .bind(body.purchase_order_id)
                .execute(&mut *ctx.conn)
                .await?;
                document::link(ctx, body.purchase_order_id, doc.id, "return").await?;
            }
        }
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.purchase_order_id, &ids).await?;

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "purchase_return_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM purchase_return_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO purchase_return_line (id, document_id, line_no, order_line_id, qty)
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

    /// The order and its vendor, and once posted what the goods returned
    /// were received for.
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
        let order = return_order(conn, doc.id).await?;
        Ok(json!({ "purchase_order_id": order, "lines": return_lines(conn, doc.id).await? }))
    }
}

pub(crate) struct PostReturn;

#[async_trait]
impl Command for PostReturn {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "purchase_return.post",
            "Post a draft purchase return: the goods leave stock at the moving average, and \
             what they were received for leaves goods received not invoiced (GRNI); a \
             difference goes to purchase price variance. Only goods received on the order and \
             not yet returned can go back. If they were invoiced, a vendor credit note clears \
             GRNI again. Posting a return that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("purchase_return:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PURCHASE_RETURN, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} return cannot be posted",
                doc.status
            )));
        }
        let order_id = return_order(&mut *ctx.conn, doc.id).await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        let lines = return_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a return needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;

        let mut movements = vec![];
        let mut inventory = vec![];
        let mut values = vec![];
        for line in &lines {
            let ordered = order_lines
                .get(&line.order_line_id)
                .ok_or_else(|| Error::internal("return line without its order line"))?;
            // Only what came from this vendor on this order can go back to
            // it: anything more would leave the order line, and GRNI, owing
            // goods that never arrived.
            if line.qty > ordered.qty_received {
                return Err(Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("qty"),
                    format_args!(
                        "{} have been received and not returned on order line {}, not {}",
                        trim(ordered.qty_received),
                        ordered.line_no,
                        trim(line.qty)
                    ),
                ));
            }
            let (item_id, warehouse_id) = ordered.stock(line.line_no)?;
            values.push(returned_value(ordered, line.qty, decimals));
            inventory.push(
                erpcraft_gl::account_for(
                    &mut *ctx.conn,
                    doc.company_id,
                    "inventory",
                    &[("item", item_id), ("warehouse", warehouse_id)],
                )
                .await?,
            );
            movements.push(StockLine {
                item_id,
                warehouse_id,
                qty: -line.qty,
                cost: None,
                source_line_id: Some(line.id),
            });
        }

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(ctx, &posting, &movements, "purchase_return").await?;

        let mut gl = vec![];
        let (mut set_values, mut set_stock_values) = (vec![], vec![]);
        for (((line, entry), inventory), value) in
            lines.iter().zip(&entries).zip(inventory).zip(values)
        {
            let stock_value = -entry.value;
            set_values.push(Some(value));
            set_stock_values.push(Some(stock_value));
            if let Some(ordered) = order_lines.get_mut(&line.order_line_id) {
                ordered.qty_received -= line.qty;
                ordered.received_value -= value;
                ordered.qty_returned += line.qty;
                ordered.returned_value += value;
            }
            if !stock_value.is_zero() {
                gl.push(gl_line(inventory, Decimal::ZERO, stock_value, line.id));
            }
            if !value.is_zero() {
                gl.push(gl_line(
                    accounts.grni_account_id,
                    value,
                    Decimal::ZERO,
                    line.id,
                ));
            }
            // The goods left stock for more than they were received for: a
            // cost. For less: a gain.
            let variance = stock_value - value;
            if !variance.is_zero() {
                gl.push(gl_line(
                    accounts.price_variance_account_id,
                    variance.max(Decimal::ZERO),
                    (-variance).max(Decimal::ZERO),
                    line.id,
                ));
            }
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "purchase_return_line",
            &ids,
            &[("value", &set_values), ("stock_value", &set_stock_values)],
        )
        .await?;
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }

        // A closed order opens again for replacements only where the
        // company wants it.
        if order.status == "closed" {
            let ordered: Decimal = order_lines.values().map(|l| l.qty).sum();
            let received: Decimal = order_lines.values().map(|l| l.qty_received).sum();
            let returning: Decimal = lines.iter().map(|l| l.qty).sum();
            let facts = policy::reopen_on_return(ordered, received, returning);
            let reopen = ctx
                .policy(policy::REOPEN_ON_RETURN, doc.company_id)
                .await?
                .evaluate(facts)?;
            if reopen == Decision::Allowed {
                document::transition(ctx, &mut order, "open").await?;
            }
        }
        settle_order(ctx, &mut order, &order_lines, false).await?;
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

pub(crate) struct ReverseReturn;

#[async_trait]
impl Command for ReverseReturn {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "purchase_return.reverse",
            "Cancel a posted purchase return entered by mistake: the goods come back into stock \
             at the value they left for, and count as received again. Refused once the vendor \
             has credited them; reverse the credit note first.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("purchase_return:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PURCHASE_RETURN, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} return cannot be reversed",
                doc.status
            )));
        }
        let order_id = return_order(&mut *ctx.conn, doc.id).await?;
        let (mut order, mut order_lines) = lock_order(ctx, order_id).await?;
        for line in return_lines(&mut *ctx.conn, doc.id).await? {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("return line without its order line"))?;
            if ordered.qty_returned - line.qty < ordered.qty_credited {
                return Err(Error::state(format!(
                    "line {}: the vendor has credited these goods; reverse the credit note first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
            let value = line.value.unwrap_or_default();
            ordered.qty_received += line.qty;
            ordered.received_value += value;
            ordered.qty_returned -= line.qty;
            ordered.returned_value -= value;
            check_still_level(ordered, line.line_no)?;
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftInput {
    /// Id of the document to draft from.
    id: Uuid,
    /// Date of the new document. Today when omitted.
    doc_date: Option<NaiveDate>,
}

fn today_or(date: Option<NaiveDate>) -> NaiveDate {
    date.unwrap_or_else(|| erpcraft_kernel::Utc::now().date_naive())
}

pub(crate) struct DraftReturn;

#[async_trait]
impl Command for DraftReturn {
    type Input = DraftInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "purchase_order.return",
            "Draft a return of goods to the vendor of a purchase order, open or closed, with a \
             line for everything received on it and not yet returned. Nothing is posted: keep \
             the lines and quantities that go back, then post the return with \
             purchase_return.post.",
        )
        .label("Return")
        .permission("purchase_return:create")
        .from_states(&["open", "closed"])
        .creates(PURCHASE_RETURN)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DocumentView> {
        let order = document::lock(ctx, PURCHASE_ORDER, input.id, "read").await?;
        if !matches!(order.status.as_str(), "open" | "closed") {
            return Err(Error::state(format!(
                "the purchase order is {}; goods are returned only against an open or closed order",
                order.status
            )));
        }
        let rows: Vec<Value> = order_lines(&mut *ctx.conn, order.id)
            .await?
            .iter()
            .filter(|l| l.qty_received > Decimal::ZERO)
            .map(|l| json!({ "order_line_id": l.id, "qty": l.qty_received }))
            .collect();
        if rows.is_empty() {
            return Err(Error::validation(
                "nothing has been received on this order that is not yet returned",
            ));
        }
        document::create(
            ctx,
            document::NewDocument {
                doc_type: PURCHASE_RETURN.into(),
                company_id: order.company_id,
                doc_date: today_or(input.doc_date),
                memo: None,
                body: json!({ "purchase_order_id": order.id, "lines": rows }),
            },
        )
        .await
    }
}

// ------------------------------------------------------- vendor credit note

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreditBody {
    vendor_invoice_id: Uuid,
    vendor_reference: Option<String>,
    #[serde(default)]
    lines: Vec<CreditLineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreditLineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    invoice_line_id: Uuid,
    qty: Decimal,
    /// The invoice line's price when omitted.
    unit_price: Option<Decimal>,
    /// The invoice line's discounts when neither is given, the amount in
    /// proportion to the quantity; zero for none.
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct CreditLine {
    id: Uuid,
    line_no: i32,
    invoice_line_id: Uuid,
    qty: Decimal,
    unit_price: Decimal,
    /// The invoice line's tax code: a credit takes tax back as charged.
    tax_code_id: Option<Uuid>,
    returned_qty: Option<Decimal>,
    cleared_value: Option<Decimal>,
    tax_amount: Option<Decimal>,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    /// From the invoice line.
    #[serde(skip)]
    order_line_id: Option<Uuid>,
    #[serde(skip)]
    invoice_qty: Decimal,
    #[serde(skip)]
    invoice_price: Decimal,
    /// What the invoice line posted, net of its discounts.
    #[serde(skip)]
    invoice_net: Decimal,
    /// Whether the line is for goods (an order line with an item).
    #[serde(skip)]
    stock: bool,
    /// The charge or account of the line, on the invoice or its order.
    #[serde(skip)]
    charge_id: Option<Uuid>,
    #[serde(skip)]
    account_id: Option<Uuid>,
}

async fn credit_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<CreditLine>> {
    let lines = sqlx::query_as(
        "SELECT c.id, c.line_no, c.invoice_line_id, c.qty, c.unit_price, i.tax_code_id,
                c.returned_qty, c.cleared_value, c.tax_amount, c.discount_percent,
                c.discount_amount, i.order_line_id, i.qty AS invoice_qty,
                i.unit_price AS invoice_price,
                COALESCE(i.net_amount, round(i.qty * i.unit_price, 2)) AS invoice_net,
                o.item_id IS NOT NULL AS stock,
                COALESCE(i.charge_id, o.charge_id) AS charge_id,
                COALESCE(i.account_id, o.account_id) AS account_id
         FROM vendor_credit_note_line c
         JOIN vendor_invoice_line i ON i.id = c.invoice_line_id
         LEFT JOIN purchase_order_line o ON o.id = i.order_line_id
         WHERE c.document_id = $1 ORDER BY c.line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

/// The invoice a credit note credits, and its order.
async fn credit_refs(conn: &mut PgConnection, document_id: Uuid) -> Result<(Uuid, Option<Uuid>)> {
    let refs = sqlx::query_as(
        "SELECT vendor_invoice_id, purchase_order_id FROM vendor_credit_note WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(refs)
}

/// How much of an invoice line posted credit notes have credited, other
/// than `except`.
async fn credited_on(
    conn: &mut PgConnection,
    invoice_line_id: Uuid,
    except: Uuid,
) -> Result<Decimal> {
    let credited = sqlx::query_scalar(
        "SELECT COALESCE(sum(c.qty), 0) FROM vendor_credit_note_line c
         JOIN document d ON d.id = c.document_id
         WHERE c.invoice_line_id = $1 AND d.status = 'posted' AND d.id <> $2",
    )
    .bind(invoice_line_id)
    .bind(except)
    .fetch_one(&mut *conn)
    .await?;
    Ok(credited)
}

/// What posted credit notes other than `except` took back of an invoice
/// line's net amount.
async fn net_credited_on(
    conn: &mut PgConnection,
    invoice_line_id: Uuid,
    except: Uuid,
) -> Result<Decimal> {
    let credited = sqlx::query_scalar(
        "SELECT COALESCE(sum(c.net_amount), 0) FROM vendor_credit_note_line c
         JOIN document d ON d.id = c.document_id
         WHERE c.invoice_line_id = $1 AND d.status = 'posted' AND d.id <> $2",
    )
    .bind(invoice_line_id)
    .bind(except)
    .fetch_one(&mut *conn)
    .await?;
    Ok(credited)
}

/// The currency of a posted vendor invoice and the rate it was posted at:
/// what a credit note for it is in, and posts at.
async fn invoice_currency(
    conn: &mut PgConnection,
    invoice: &Document,
) -> Result<(String, Decimal)> {
    let (currency, rate): (String, Option<Decimal>) = sqlx::query_as(
        "SELECT COALESCE(o.currency, v.currency, c.base_currency), v.fx_rate FROM vendor_invoice v
         LEFT JOIN purchase_order o ON o.document_id = v.purchase_order_id
         JOIN company c ON c.id = $2
         WHERE v.document_id = $1",
    )
    .bind(invoice.id)
    .bind(invoice.company_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok((currency, rate.unwrap_or(Decimal::ONE)))
}

/// Whether posted credit notes credit any line of this invoice.
pub(crate) async fn is_credited(conn: &mut PgConnection, invoice_id: Uuid) -> Result<bool> {
    let credited = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM vendor_credit_note_line c
            JOIN document d ON d.id = c.document_id
            JOIN vendor_invoice_line i ON i.id = c.invoice_line_id
            WHERE i.document_id = $1 AND d.status = 'posted')",
    )
    .bind(invoice_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(credited)
}

pub(crate) struct VendorCreditNote;

#[async_trait]
impl DocumentHandler for VendorCreditNote {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: VENDOR_CREDIT_NOTE.into(),
            module: String::new(),
            label: "Vendor credit note".into(),
            number_prefix: "VC".into(),
            fields: vec![
                FieldMeta::new(
                    "vendor_invoice_id",
                    "Vendor invoice",
                    FieldType::document_in(VENDOR_INVOICE, &["posted"]),
                )
                .required()
                .immutable(),
                FieldMeta::new(
                    "purchase_order_id",
                    "Purchase order",
                    FieldType::document(PURCHASE_ORDER),
                )
                .readonly(),
                FieldMeta::new(
                    "vendor_reference",
                    "Vendor's credit note number",
                    FieldType::Text,
                ),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly()
                .help("The invoice's currency."),
                FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
                    .rate(10)
                    .readonly()
                    .help("The invoice's rate: a credit note takes back what the invoice booked."),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new(
                        "invoice_line_id",
                        "Invoice line",
                        FieldType::line_of("vendor_invoice_id", VENDOR_INVOICE, "lines"),
                    )
                    .required(),
                    FieldMeta::new("qty", "Quantity credited", FieldType::Decimal)
                        .quantity()
                        .required(),
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .currency_of_document("currency")
                        .help("The invoice line's price when left out."),
                ]
                .into_iter()
                .chain(erpcraft_tax::lines::discount_fields())
                .chain([
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                        .readonly()
                        .help("The invoice line's: tax is taken back as it was charged."),
                    FieldMeta::new("returned_qty", "Of it returned", FieldType::Decimal)
                        .quantity()
                        .readonly(),
                    FieldMeta::new("tax_amount", "Tax", FieldType::Decimal)
                        .money()
                        .currency_of_document("currency")
                        .readonly(),
                    FieldMeta::new(
                        "cleared_value",
                        "Value of the goods credited",
                        FieldType::Decimal,
                    )
                    .money()
                    .readonly(),
                ])
                .collect(),
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
            summary: vec![
                FieldMeta::new(
                    "vendor_invoice_id",
                    "Vendor invoice",
                    FieldType::document(VENDOR_INVOICE),
                )
                .readonly(),
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
                    .readonly(),
                FieldMeta::new("total_gross", "Total", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly(),
            ]
            .into_iter()
            .chain(erpcraft_tax::lines::discount_and_charge_fields())
            .chain(crate::accounting_fields())
            .collect(),
            // 1: currency, accounting totals and their declarations.
            // 2: discounts and charges.
            summary_version: 2,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: CreditBody =
            erpcraft_kernel::input::parse(body.clone(), "vendor credit note body")?;
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            if line.qty <= Decimal::ZERO {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("qty"),
                    "the quantity must be more than zero",
                ));
            }
            if let Some(price) = line.unit_price {
                if price.is_sign_negative() {
                    return Err(Error::line(
                        "lines",
                        n,
                        Some("unit_price"),
                        "the unit price cannot be negative",
                    ));
                }
                if price.normalize().scale() > 6 {
                    return Err(Error::line(
                        "lines",
                        n,
                        Some("unit_price"),
                        "a unit price has at most 6 decimal places",
                    ));
                }
            }
        }
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT vendor_invoice_id FROM vendor_credit_note WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        match existing {
            Some(current) if current != body.vendor_invoice_id => {
                return Err(Error::validation(
                    "the invoice of a credit note cannot change; create a new credit note instead",
                ));
            }
            Some(_) => {}
            None => {
                let invoice: Option<(String, Option<Uuid>)> = sqlx::query_as(
                    "SELECT d.status, v.purchase_order_id FROM document d
                     JOIN vendor_invoice v ON v.document_id = d.id
                     WHERE d.id = $1 AND d.company_id = $2",
                )
                .bind(body.vendor_invoice_id)
                .bind(doc.company_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
                let order_id = match invoice {
                    None => {
                        return Err(Error::validation(
                            "vendor_invoice_id must be a vendor invoice of the document's company",
                        )
                        .with_field("vendor_invoice_id"));
                    }
                    Some((status, _)) if status != "posted" => {
                        return Err(Error::state(format!(
                            "the vendor invoice is {status}; only a posted invoice can be credited"
                        )));
                    }
                    Some((_, order_id)) => order_id,
                };
                sqlx::query(
                    "INSERT INTO vendor_credit_note (document_id, vendor_invoice_id, purchase_order_id)
                     VALUES ($1, $2, $3)",
                )
                .bind(doc.id)
                .bind(body.vendor_invoice_id)
                .bind(order_id)
                .execute(&mut *ctx.conn)
                .await?;
                document::link(ctx, body.vendor_invoice_id, doc.id, "credit note").await?;
            }
        }
        let reference = body
            .vendor_reference
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        sqlx::query("UPDATE vendor_credit_note SET vendor_reference = $2 WHERE document_id = $1")
            .bind(doc.id)
            .bind(reference)
            .execute(&mut *ctx.conn)
            .await?;

        // Every line credits a line of the invoice, once, at its price
        // unless another is given.
        let mut ids: Vec<Uuid> = body.lines.iter().map(|l| l.invoice_line_id).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != body.lines.len() {
            return Err(Error::validation(
                "an invoice line can appear only once in a credit note",
            ));
        }
        #[allow(clippy::type_complexity)]
        let invoiced: Vec<(Uuid, Decimal, Decimal, Option<Decimal>, Option<Decimal>)> =
            sqlx::query_as(
                "SELECT id, unit_price, qty, discount_percent, discount_amount
                 FROM vendor_invoice_line WHERE id = ANY($1) AND document_id = $2",
            )
            .bind(&ids)
            .bind(body.vendor_invoice_id)
            .fetch_all(&mut *ctx.conn)
            .await?;
        if invoiced.len() != ids.len() {
            return Err(Error::validation(
                "every line must refer to a line of the vendor invoice",
            ));
        }
        let prices: Vec<(Uuid, Decimal)> = invoiced.iter().map(|i| (i.0, i.1)).collect();
        // The invoice line's discounts unless the credit gives its own.
        let decimals = {
            let currency: String = sqlx::query_scalar(
                "SELECT COALESCE(o.currency, v.currency, c.base_currency) FROM vendor_invoice v
                 LEFT JOIN purchase_order o ON o.document_id = v.purchase_order_id
                 JOIN document d ON d.id = v.document_id JOIN company c ON c.id = d.company_id
                 WHERE v.document_id = $1",
            )
            .bind(body.vendor_invoice_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?
        };
        let mut discounts: Vec<(Option<Decimal>, Option<Decimal>)> = vec![];
        for (index, line) in body.lines.iter().enumerate() {
            let (percent, amount) = match (line.discount_percent, line.discount_amount) {
                (None, None) => invoiced
                    .iter()
                    .find(|i| i.0 == line.invoice_line_id)
                    .map(|i| (i.3, i.4.map(|a| round_money(a * line.qty / i.2, decimals))))
                    .unwrap_or_default(),
                given => given,
            };
            if percent.is_some_and(|p| p < Decimal::ZERO || p > Decimal::ONE_HUNDRED)
                || amount.is_some_and(|a| a < Decimal::ZERO)
            {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("discount_percent"),
                    "a discount percent is from 0 to 100 and an amount not below zero",
                ));
            }
            discounts.push((
                erpcraft_tax::lines::no_zero(percent),
                erpcraft_tax::lines::no_zero(amount),
            ));
        }

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "vendor_credit_note_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM vendor_credit_note_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO vendor_credit_note_line (id, document_id, line_no, invoice_line_id, qty,
                    unit_price, discount_percent, discount_amount)
                 SELECT r.id, $1, r.line_no, r.invoice_line_id, r.qty, r.unit_price,
                    r.discount_percent, r.discount_amount
                 FROM unnest($2::uuid[], $3::int4[], $4::uuid[], $5::numeric[], $6::numeric[],
                             $7::numeric[], $8::numeric[])
                    AS r (id, line_no, invoice_line_id, qty, unit_price, discount_percent,
                          discount_amount)",
            )
            .bind(doc.id)
            .bind(&line_ids)
            .bind(&line_nos)
            .bind(body.lines.iter().map(|line| line.invoice_line_id).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.qty).collect::<Vec<_>>())
            .bind(body.lines.iter().map(|line| line.unit_price.unwrap_or_else(|| {
                        prices
                            .iter()
                            .find(|(id, _)| *id == line.invoice_line_id)
                            .map(|(_, price)| *price)
                            .unwrap_or_default()
                    })).collect::<Vec<_>>())
            .bind(discounts.iter().map(|d| d.0).collect::<Vec<_>>())
            .bind(discounts.iter().map(|d| d.1).collect::<Vec<_>>())
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The invoice, its order and vendor, and what the credit comes to:
    /// net, the tax taken back, and both together, as positive amounts.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        use document::body::{decimal, uuid};
        let mut summary = follows(conn, body).await?;
        let decimals = match body["currency"].as_str() {
            Some(currency) => erpcraft_gl::currency_decimals(conn, currency).await?,
            None => company::currency_decimals(conn, doc.company_id).await?,
        };
        let mut net = Decimal::ZERO;
        let mut tax = Some(Decimal::ZERO);
        for line in document::body::lines(body, "lines") {
            let credited = erpcraft_tax::lines::body_line_net(line, decimals);
            net += credited;
            let on_line = match decimal(&line["tax_amount"]) {
                Some(recorded) => Some(recorded),
                None if uuid(&line["tax_code_id"]).is_none() => Some(Decimal::ZERO),
                None => match uuid(&line["invoice_line_id"]) {
                    Some(invoice_line) => erpcraft_tax::credit(
                        &mut *conn,
                        doc.company_id,
                        invoice_line,
                        credited,
                        uuid(&line["id"]),
                        decimal(&body["fx_rate"]).unwrap_or(Decimal::ONE),
                    )
                    .await
                    .ok()
                    .map(|taxes| -erpcraft_tax::Tax::total(&taxes)),
                    None => None,
                },
            };
            tax = tax.zip(on_line).map(|(tax, on_line)| tax + on_line);
        }
        summary["currency"] = body["currency"].clone();
        summary["vendor_invoice_id"] = body["vendor_invoice_id"].clone();
        summary["total_net"] = json!(net);
        summary["total_tax"] = json!(tax);
        summary["total_gross"] = json!(tax.map(|tax| net + tax));
        // Charges: credited lines of the invoice that were charges, on the
        // invoice or on its order.
        let lines = document::body::lines(body, "lines");
        let credited: Vec<Uuid> = lines
            .iter()
            .filter_map(|l| uuid(&l["invoice_line_id"]))
            .collect();
        let charge_lines: Vec<Uuid> = sqlx::query_scalar(
            "SELECT i.id FROM vendor_invoice_line i LEFT JOIN purchase_order_line o ON o.id = i.order_line_id
             WHERE i.id = ANY($1) AND COALESCE(i.charge_id, o.charge_id) IS NOT NULL",
        )
        .bind(&credited)
        .fetch_all(&mut *conn)
        .await?;
        let (discount, charges) =
            erpcraft_tax::lines::discount_and_charges(lines, decimals, |line| {
                uuid(&line["invoice_line_id"]).is_some_and(|id| charge_lines.contains(&id))
            });
        summary["total_discount"] = json!(discount);
        summary["total_charges"] = json!(charges);
        let base = company::base_currency(conn, doc.company_id).await?;
        let foreign = body["currency"].as_str().is_some_and(|c| c != base);
        let totals = crate::accounting_totals(conn, doc, foreign, net, tax).await?;
        crate::add_totals(&mut summary, totals);
        Ok(summary)
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (invoice, order) = credit_refs(conn, doc.id).await?;
        let reference: Option<String> = sqlx::query_scalar(
            "SELECT vendor_reference FROM vendor_credit_note WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *conn)
        .await?;
        // In the invoice's currency, at the invoice's rate.
        let invoice_doc: Option<Document> = sqlx::query_as("SELECT * FROM document WHERE id = $1")
            .bind(invoice)
            .fetch_optional(&mut *conn)
            .await?;
        let (currency, rate) = match &invoice_doc {
            Some(invoice) => invoice_currency(conn, invoice).await?,
            None => (
                company::base_currency(conn, doc.company_id).await?,
                Decimal::ONE,
            ),
        };
        Ok(json!({
            "vendor_invoice_id": invoice,
            "purchase_order_id": order,
            "vendor_reference": reference,
            "currency": currency,
            "fx_rate": rate,
            "lines": credit_lines(conn, doc.id).await?,
        }))
    }
}

pub(crate) struct PostCreditNote;

#[async_trait]
impl Command for PostCreditNote {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "vendor_credit_note.post",
            "Post a draft vendor credit note. It reduces what is owed to the vendor: an open \
             item below zero, settled with the vendor's invoices by a payment or applied to one \
             with payment.apply_credit. For goods returned and not yet credited it puts back \
             into goods received not invoiced what they were invoiced at; a credit beyond what \
             was returned is for price only and refused unless the company allows it. Tax is \
             taken back at the invoice's code and rate, in this credit note's period. Posting \
             a credit note that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("vendor_credit_note:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, VENDOR_CREDIT_NOTE, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} credit note cannot be posted",
                doc.status
            )));
        }
        let (invoice_id, order_id) = credit_refs(&mut *ctx.conn, doc.id).await?;
        // The invoice's order; none for a direct invoice.
        let (mut order, mut order_lines) = match order_id {
            Some(order_id) => {
                let (order, lines) = lock_order(ctx, order_id).await?;
                (Some(order), lines)
            }
            None => (None, BTreeMap::new()),
        };
        let invoice = document::lock(ctx, VENDOR_INVOICE, invoice_id, "read").await?;
        if invoice.status != "posted" {
            return Err(Error::state(format!(
                "the vendor invoice is {}; only a posted invoice can be credited",
                invoice.status
            )));
        }
        let lines = credit_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a credit note needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        let base_decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        // A credit note is in the invoice's currency and posts at the
        // invoice's rate, so it takes back exactly what the invoice put in
        // the books; an exchange difference arises only when money moves.
        let (currency, rate) = invoice_currency(&mut *ctx.conn, &invoice).await?;
        let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?;
        let conversion = (currency
            != company::base_currency(&mut *ctx.conn, doc.company_id).await?)
            .then(|| erpcraft_tax::Conversion {
                currency: currency.clone(),
                rate,
            });
        let without_return = ctx
            .policy(policy::CREDIT_WITHOUT_RETURN, doc.company_id)
            .await?;
        // The share of the invoice the company kept back as early-payment
        // discount was never paid: of what is credited, that share comes
        // off the discount received rather than being owed back, and tax
        // is taken back only on the rest, as the discount took the tax on
        // it.
        let discount_share = erpcraft_openitems::discount_share(&mut *ctx.conn, invoice.id).await?;
        let discount_account = if discount_share.is_zero() {
            None
        } else {
            Some(
                erpcraft_gl::account_for(&mut *ctx.conn, doc.company_id, "discount_received", &[])
                    .await?,
            )
        };

        let mut owed_back_value = Decimal::ZERO;
        let mut owed_back = Decimal::ZERO;
        let mut taxes = vec![];
        let mut gl = vec![];
        let (mut set_returned, mut set_cleared, mut set_tax) = (vec![], vec![], vec![]);
        let mut set_net = vec![];
        for line in &lines {
            let credited_before = credited_on(&mut *ctx.conn, line.invoice_line_id, doc.id).await?;
            let left = line.invoice_qty - credited_before;
            if line.qty > left {
                return Err(Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("qty"),
                    format_args!(
                        "{} of what the invoice line charged are not yet credited, not {}",
                        trim(left),
                        trim(line.qty)
                    ),
                ));
            }
            // Goods returned are credited first; a line that is not stock
            // has nothing to come back.
            let (with_return, cleared) = match line
                .order_line_id
                .and_then(|id| order_lines.get_mut(&id))
                .filter(|_| line.stock)
            {
                None => (Decimal::ZERO, Decimal::ZERO),
                Some(ordered) => {
                    let returned = (ordered.qty_returned - ordered.qty_credited).max(Decimal::ZERO);
                    let facts = policy::credit_without_return(
                        line.qty,
                        returned,
                        line.invoice_price,
                        line.unit_price,
                    );
                    if let Decision::Refused(refusal) = without_return.evaluate(facts)? {
                        let why = refusal.message.unwrap_or_else(|| {
                    format!(
                        "{} of the goods on order line {} have been returned and not credited, not {}; a credit without a return is not accepted",
                        trim(returned),
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
                    // The returned goods are credited first; the rest is price.
                    let with_return = line.qty.min(returned);
                    let cleared = credited_value(ordered, with_return, base_decimals);
                    ordered.qty_invoiced -= with_return;
                    ordered.cleared_value -= cleared;
                    ordered.qty_credited += with_return;
                    ordered.credited_value += cleared;
                    (with_return, cleared)
                }
            };

            let credited = erpcraft_tax::line_net(
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
            // Never more than the invoice line charged, less what other
            // credit notes took back of it. The database holds to the same.
            let charged = line.invoice_net;
            let left_to_credit =
                charged - net_credited_on(&mut *ctx.conn, line.invoice_line_id, doc.id).await?;
            if credited > left_to_credit {
                return Err(Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("unit_price"),
                    format_args!(
                        "{} of what the invoice line charged is left to credit, not {}",
                        left_to_credit.normalize(),
                        credited.normalize()
                    ),
                ));
            }
            set_net.push(Some(credited));
            let credited_value = round_money(credited * rate, base_decimals);
            let paid = round_money(credited * (Decimal::ONE - discount_share), decimals);
            let discount_back = credited - paid;
            owed_back += paid;
            owed_back_value += round_money(paid * rate, base_decimals);
            let mut tax_back = Decimal::ZERO;
            if line.tax_code_id.is_some() {
                // Each tax the invoice charged on the line, each from its
                // own account.
                for tax in erpcraft_tax::credit(
                    &mut *ctx.conn,
                    doc.company_id,
                    line.invoice_line_id,
                    paid,
                    Some(line.id),
                    rate,
                )
                .await?
                {
                    let back = -tax.amount;
                    tax_back += back;
                    if !back.is_zero() {
                        owed_back += back;
                        owed_back_value += round_money(back * rate, base_decimals);
                        gl.push(gl_line(
                            tax.account_id,
                            Decimal::ZERO,
                            -tax.value(),
                            line.id,
                        ));
                        gl.push(in_currency(
                            gl_line(accounts.payable_account_id, back, Decimal::ZERO, line.id),
                            conversion.as_ref(),
                        ));
                    }
                    taxes.push(tax);
                }
            }
            set_returned.push(Some(with_return));
            set_cleared.push(Some(cleared));
            set_tax.push(Some(tax_back));

            if !paid.is_zero() {
                gl.push(in_currency(
                    gl_line(accounts.payable_account_id, paid, Decimal::ZERO, line.id),
                    conversion.as_ref(),
                ));
            }
            if let Some(account) = discount_account.filter(|_| !discount_back.is_zero()) {
                gl.push(in_currency(
                    gl_line(account, discount_back, Decimal::ZERO, line.id),
                    conversion.as_ref(),
                ));
            }
            if !cleared.is_zero() {
                gl.push(gl_line(
                    accounts.grni_account_id,
                    Decimal::ZERO,
                    cleared,
                    line.id,
                ));
            }
            // Credited for more than the goods were invoiced at: a gain.
            // For less: a cost. A credit for price only is all variance.
            let variance = credited_value - cleared;
            // For goods, price variance; for a line that is not stock, the
            // account it was a cost of (a landed-cost charge credited
            // after it was allocated: price variance, or the account for
            // `landed_cost_variance`).
            let variance_account = if line.stock {
                accounts.price_variance_account_id
            } else {
                let landed: bool = match line.charge_id.filter(|_| line.order_line_id.is_none()) {
                    Some(charge) => {
                        sqlx::query_scalar("SELECT landed_cost FROM charge WHERE id = $1")
                            .bind(charge)
                            .fetch_one(&mut *ctx.conn)
                            .await?
                    }
                    None => false,
                };
                if landed {
                    erpcraft_gl::account_for(
                        &mut *ctx.conn,
                        doc.company_id,
                        "landed_cost_variance",
                        &[],
                    )
                    .await
                    .unwrap_or(accounts.price_variance_account_id)
                } else {
                    crate::expense_account(
                        &mut *ctx.conn,
                        doc.company_id,
                        line.charge_id,
                        line.account_id,
                    )
                    .await?
                }
            };
            if !variance.is_zero() {
                gl.push(gl_line(
                    variance_account,
                    (-variance).max(Decimal::ZERO),
                    variance.max(Decimal::ZERO),
                    line.id,
                ));
            }
        }

        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "vendor_credit_note_line",
            &ids,
            &[
                ("returned_qty", &set_returned),
                ("cleared_value", &set_cleared),
                ("tax_amount", &set_tax),
                ("net_amount", &set_net),
            ],
        )
        .await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        // The last of an invoice's tax in a foreign currency takes the last
        // of its accounting value, which can differ by a cent from what
        // its amount is worth at the rate.
        if let Some(rounding) =
            erpcraft_gl::exchange_difference(&mut *ctx.conn, doc.company_id, &gl, None).await?
        {
            gl.push(rounding);
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        if conversion.is_some() {
            sqlx::query("UPDATE vendor_credit_note SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }
        // What the vendor now owes back, as a credit against its invoices.
        if !owed_back.is_zero() {
            // The party of the invoice: its order's, or its own.
            let party_id: Uuid = sqlx::query_scalar(
                "SELECT COALESCE(i.vendor_id, o.vendor_id) FROM vendor_invoice i
                 LEFT JOIN purchase_order o ON o.document_id = i.purchase_order_id
                 WHERE i.document_id = $1",
            )
            .bind(invoice_id)
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
                    amount: -owed_back_value,
                    // A credit is due back at once: on its own date.
                    due_date: Some(posting.posting_date),
                    terms: None,
                    foreign: conversion
                        .as_ref()
                        .map(|c| (c.currency.clone(), -owed_back)),
                },
            )
            .await?;
        }
        if let Some(order) = &mut order {
            settle_order(ctx, order, &order_lines, false).await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

pub(crate) struct ReverseCreditNote;

#[async_trait]
impl Command for ReverseCreditNote {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "vendor_credit_note.reverse",
            "Cancel a posted vendor credit note entered by mistake. What it credited is owed \
             again, and the returned goods it covered wait for a credit again. Refused once the \
             credit has been settled or applied; reverse that payment first.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("vendor_credit_note:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, VENDOR_CREDIT_NOTE, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} credit note cannot be reversed",
                doc.status
            )));
        }
        let (_, order_id) = credit_refs(&mut *ctx.conn, doc.id).await?;
        // The invoice's order; none for a direct invoice.
        let (mut order, mut order_lines) = match order_id {
            Some(order_id) => {
                let (order, lines) = lock_order(ctx, order_id).await?;
                (Some(order), lines)
            }
            None => (None, BTreeMap::new()),
        };
        for line in credit_lines(&mut *ctx.conn, doc.id).await? {
            let Some(ordered) = line.order_line_id.and_then(|id| order_lines.get_mut(&id)) else {
                continue;
            };
            let with_return = line.returned_qty.unwrap_or_default();
            let cleared = line.cleared_value.unwrap_or_default();
            ordered.qty_invoiced += with_return;
            ordered.cleared_value += cleared;
            ordered.qty_credited -= with_return;
            ordered.credited_value -= cleared;
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

pub(crate) struct DraftCreditNote;

#[async_trait]
impl Command for DraftCreditNote {
    type Input = DraftInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "vendor_invoice.credit",
            "Draft a credit note for a posted vendor invoice, with a line for each of its lines \
             whose goods were returned and not yet credited, at the invoice's price. Nothing is \
             posted: set the quantities and prices the vendor credits and its credit note \
             number, then post it with vendor_credit_note.post.",
        )
        .label("Credit")
        .permission("vendor_credit_note:create")
        .from_states(&["posted"])
        .creates(VENDOR_CREDIT_NOTE)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DocumentView> {
        let invoice = document::lock(ctx, VENDOR_INVOICE, input.id, "read").await?;
        if invoice.status != "posted" {
            return Err(Error::state(format!(
                "the vendor invoice is {}; only a posted invoice can be credited",
                invoice.status
            )));
        }
        let order_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT purchase_order_id FROM vendor_invoice WHERE document_id = $1",
        )
        .bind(invoice.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        // A direct invoice has no order, and nothing came back on one.
        let order = match order_id {
            Some(order_id) => order_lines(&mut *ctx.conn, order_id).await?,
            None => vec![],
        };
        let invoice_lines: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
            "SELECT id, order_line_id, qty FROM vendor_invoice_line
             WHERE document_id = $1 AND order_line_id IS NOT NULL ORDER BY line_no",
        )
        .bind(invoice.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut rows = vec![];
        for (id, order_line_id, qty) in invoice_lines {
            let left = qty - credited_on(&mut *ctx.conn, id, Uuid::nil()).await?;
            let returned = order
                .iter()
                .find(|l| l.id == order_line_id)
                .map_or(Decimal::ZERO, |l| l.qty_returned - l.qty_credited);
            let qty = left.min(returned);
            if qty > Decimal::ZERO {
                rows.push(json!({ "invoice_line_id": id, "qty": qty }));
            }
        }
        if rows.is_empty() {
            return Err(Error::validation(
                "nothing returned on this invoice's lines is waiting for a credit; a credit for \
                 price only is entered line by line",
            ));
        }
        document::create(
            ctx,
            document::NewDocument {
                doc_type: VENDOR_CREDIT_NOTE.into(),
                company_id: invoice.company_id,
                doc_date: today_or(input.doc_date),
                memo: None,
                body: json!({ "vendor_invoice_id": invoice.id, "lines": rows }),
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(received: (i64, i64), invoiced: (i64, i64)) -> OrderLine {
        OrderLine {
            id: Uuid::nil(),
            line_no: 1,
            item_id: Some(Uuid::nil()),
            warehouse_id: Some(Uuid::nil()),
            charge_id: None,
            account_id: None,
            description: None,
            qty: Decimal::from(10),
            unit_price: Decimal::ZERO,
            discount_percent: None,
            discount_amount: None,
            qty_received: Decimal::from(received.0),
            received_value: Decimal::new(received.1, 2),
            qty_invoiced: Decimal::from(invoiced.0),
            cleared_value: Decimal::new(invoiced.1, 2),
            qty_returned: Decimal::ZERO,
            returned_value: Decimal::ZERO,
            qty_credited: Decimal::ZERO,
            credited_value: Decimal::ZERO,
            net_amount: None,
        }
    }

    #[test]
    fn what_levels_a_line_takes_what_is_left() {
        // 3 received for 10.00, nothing invoiced: one goes back at a third,
        // the last at what is left.
        let l = line((3, 1000), (0, 0));
        assert_eq!(returned_value(&l, Decimal::ONE, 2), Decimal::new(333, 2));
        assert_eq!(
            returned_value(&l, Decimal::from(3), 2),
            Decimal::new(1000, 2)
        );
        // 3 received and 1 invoiced (cleared 3.33): returning 2 levels it.
        let l = line((3, 1000), (1, 333));
        assert_eq!(
            returned_value(&l, Decimal::from(2), 2),
            Decimal::new(667, 2)
        );
        // 1 kept, 3 invoiced (2 returned, not credited): crediting 2 levels it.
        let l = line((1, 333), (3, 1000));
        assert_eq!(
            credited_value(&l, Decimal::from(2), 2),
            Decimal::new(667, 2)
        );
        assert_eq!(credited_value(&l, Decimal::ZERO, 2), Decimal::ZERO);
    }
}
