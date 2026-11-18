//! Goods back from a customer, and the customer's credit for them.
//!
//! A **sales return** brings goods delivered against an order back into
//! stock at the cost they left for: the average cost of what was delivered
//! on the order line and has not come back. Cost of goods sold is taken
//! back by exactly that, so a delivery and its full return leave nothing
//! behind.
//!
//! A **sales credit note** reduces what the customer owes for a posted
//! invoice: revenue and the tax charged are taken back. The part of a line
//! covered by goods that came back and are not yet credited counts on the
//! order line; any other part is a credit of price only, refused unless the
//! company allows it (`sales_credit_note.without_return`). The credit is an
//! open item below zero, which a payment settles with the customer's
//! invoices, or `payment.apply_credit` applies to one of them. Tax is taken
//! back at the invoice's code and rate, in the credit note's own period.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, Decision, DocTypeMeta, Error, FieldMeta,
    FieldType, JsonSchema, PgConnection, Result, StateMeta, async_trait, company, round_money,
    schemars,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use erpcraft_stock::{Cost, StockLine};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    DocInput, ReverseInput, SALES_INVOICE, SALES_ORDER, check_order_lines, follows, gl_line,
    in_currency, lock_order, lock_order_stock, order_lines, policy, post_and_reverse, settings,
    settle_order, states, trim,
};

pub(crate) const SALES_RETURN: &str = "sales_return";
pub(crate) const SALES_CREDIT_NOTE: &str = "sales_credit_note";

/// What the goods of an order line that went out and have not come back
/// left stock for, together.
pub(crate) async fn cost_out(conn: &mut PgConnection, order_line_id: Uuid) -> Result<Decimal> {
    let cost = sqlx::query_scalar(
        "SELECT
            COALESCE((SELECT sum(l.value) FROM sales_delivery_line l
                      JOIN document d ON d.id = l.document_id
                      WHERE l.order_line_id = $1 AND d.status = 'posted'), 0)
          - COALESCE((SELECT sum(l.value) FROM sales_return_line l
                      JOIN document d ON d.id = l.document_id
                      WHERE l.order_line_id = $1 AND d.status = 'posted'), 0)",
    )
    .bind(order_line_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(cost)
}

/// What `qty` coming back of `out` units that cost `cost` together are
/// worth: their share, the last of them all that is left.
fn cost_back(qty: Decimal, out: Decimal, cost: Decimal, decimals: u32) -> Decimal {
    if qty == out {
        cost
    } else {
        round_money(qty * cost / out, decimals)
            .min(cost)
            .max(Decimal::ZERO)
    }
}

async fn check_returnable_order(ctx: &mut Ctx<'_>, doc: &Document, order_id: Uuid) -> Result<()> {
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
        Some("open" | "closed") => Ok(()),
        Some(status) => Err(Error::state(format!(
            "the sales order is {status}; goods come back only against an open or closed order"
        ))),
    }
}

// ------------------------------------------------------------- sales return

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReturnBody {
    sales_order_id: Uuid,
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
}

async fn return_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<ReturnLine>> {
    let lines = sqlx::query_as(
        "SELECT id, line_no, order_line_id, qty, value FROM sales_return_line
         WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

async fn return_order(conn: &mut PgConnection, document_id: Uuid) -> Result<Uuid> {
    let order =
        sqlx::query_scalar("SELECT sales_order_id FROM sales_return WHERE document_id = $1")
            .bind(document_id)
            .fetch_one(&mut *conn)
            .await?;
    Ok(order)
}

pub(crate) struct SalesReturn;

#[async_trait]
impl DocumentHandler for SalesReturn {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: SALES_RETURN.into(),
            module: String::new(),
            label: "Sales return".into(),
            number_prefix: "SR".into(),
            fields: vec![
                FieldMeta::new(
                    "sales_order_id",
                    "Sales order",
                    FieldType::document_in(SALES_ORDER, &["open", "closed"]),
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
                    FieldMeta::new("qty", "Quantity returned", FieldType::Decimal)
                        .quantity()
                        .required(),
                    FieldMeta::new("value", "Cost", FieldType::Decimal)
                        .money()
                        .readonly()
                        .help("Set when posted: the cost the goods left stock for."),
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
                FieldMeta::new("total_cost", "Cost", FieldType::Decimal)
                    .money()
                    .readonly()
                    .help("Known once posted: the cost the goods came back at."),
            ],
            // 1: currency, accounting totals and their declarations.
            summary_version: 1,
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: ReturnBody = erpcraft_kernel::input::parse(body.clone(), "sales return body")?;
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
        let existing: Option<Uuid> =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_return WHERE document_id = $1")
                .bind(doc.id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        match existing {
            Some(current) if current != body.sales_order_id => {
                return Err(Error::validation(
                    "the sales order of a document cannot change; create a new document instead",
                ));
            }
            Some(_) => {}
            None => {
                check_returnable_order(ctx, doc, body.sales_order_id).await?;
                sqlx::query(
                    "INSERT INTO sales_return (document_id, sales_order_id) VALUES ($1, $2)",
                )
                .bind(doc.id)
                .bind(body.sales_order_id)
                .execute(&mut *ctx.conn)
                .await?;
                document::link(ctx, body.sales_order_id, doc.id, "return").await?;
            }
        }
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.order_line_id).collect();
        check_order_lines(ctx, body.sales_order_id, &ids).await?;

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "sales_return_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_return_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO sales_return_line (id, document_id, line_no, order_line_id, qty)
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
        summary["total_cost"] = json!(total.filter(|_| !lines.is_empty()));
        Ok(summary)
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let order = return_order(conn, doc.id).await?;
        Ok(json!({ "sales_order_id": order, "lines": return_lines(conn, doc.id).await? }))
    }
}

pub(crate) struct PostReturn;

#[async_trait]
impl Command for PostReturn {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_return.post",
            "Post a draft sales return: the goods come back into stock at the cost they left \
             for, and cost of goods sold is taken back by the same. Only goods delivered on the \
             order and not yet returned can come back. If they were invoiced, a sales credit \
             note takes the revenue back. Posting a return that is already posted returns it \
             unchanged.",
        )
        .from_states(&["draft"])
        .permission("sales_return:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_RETURN, input.id, "post").await?;
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
        lock_order_stock(ctx, &order, &order_lines).await?;
        let lines = return_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a return needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;

        let mut movements = vec![];
        let mut inventory = vec![];
        for line in &lines {
            let ordered = order_lines
                .get(&line.order_line_id)
                .ok_or_else(|| Error::internal("return line without its order line"))?;
            let (item_id, warehouse_id) = ordered.stock(line.line_no)?;
            // Only what went out on this order can come back on it: more
            // would take back cost that was never charged.
            if line.qty > ordered.qty_delivered {
                return Err(Error::line(
                    "lines",
                    line.line_no as usize,
                    Some("qty"),
                    format_args!(
                        "{} have been delivered and not returned on order line {}, not {}",
                        trim(ordered.qty_delivered),
                        ordered.line_no,
                        trim(line.qty)
                    ),
                ));
            }
            let out = cost_out(&mut *ctx.conn, ordered.id).await?;
            let cost = cost_back(line.qty, ordered.qty_delivered, out, decimals);
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
                qty: line.qty,
                cost: Some(Cost::Total(cost)),
                source_line_id: Some(line.id),
            });
        }

        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        let entries = erpcraft_stock::post(ctx, &posting, &movements, "sales_return").await?;

        let mut gl = vec![];
        let mut costs = vec![];
        for ((line, entry), inventory) in lines.iter().zip(&entries).zip(inventory) {
            let cost = entry.cost();
            costs.push(Some(cost));
            if let Some(ordered) = order_lines.get_mut(&line.order_line_id) {
                ordered.qty_delivered -= line.qty;
                ordered.qty_returned += line.qty;
            }
            if !entry.value.is_zero() {
                gl.push(gl_line(inventory, entry.value, Decimal::ZERO, line.id));
            }
            // Goods that came back onto stock below zero: the part of their
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
            if !cost.is_zero() {
                gl.push(gl_line(
                    accounts.cogs_account_id,
                    Decimal::ZERO,
                    cost,
                    line.id,
                ));
            }
        }
        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "sales_return_line",
            &ids,
            &[("value", &costs)],
        )
        .await?;
        if !gl.is_empty() {
            let scopes = erpcraft_stock::line_scopes(&mut *ctx.conn, &entries).await?;
            let customer = crate::order_customer(&mut *ctx.conn, order.id).await?;
            erpcraft_gl::add_scopes(&mut gl, &scopes, &[("customer", customer)]);
            erpcraft_gl::post(ctx, &posting, &gl).await?;
        }

        if order.status == "closed" {
            let ordered: Decimal = order_lines.values().map(|l| l.qty).sum();
            let delivered: Decimal = order_lines.values().map(|l| l.qty_delivered).sum();
            let returning: Decimal = lines.iter().map(|l| l.qty).sum();
            let facts = policy::reopen_on_return(ordered, delivered, returning);
            let reopen = ctx
                .policy(policy::REOPEN_ON_RETURN, doc.company_id)
                .await?
                .evaluate(facts)?;
            if reopen == Decision::Allowed {
                document::transition(ctx, &mut order, "open").await?;
            }
        }
        settle_order(ctx, &mut order, &order_lines, false).await?;
        // The parties as the order showed them, kept from now on.
        let customer: Uuid =
            sqlx::query_scalar("SELECT customer_id FROM sales_order WHERE document_id = $1")
                .bind(order_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        crate::keep_parties(&mut *ctx.conn, &doc, order_id, customer).await?;
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
            "sales_return.reverse",
            "Cancel a posted sales return entered by mistake: the goods leave stock again at \
             the cost they came back at, and count as delivered again. Refused once the \
             customer has been credited for them (reverse the credit note first), or once the \
             goods have moved on.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("sales_return:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_RETURN, input.id, "reverse").await?;
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
        lock_order_stock(ctx, &order, &order_lines).await?;
        for line in return_lines(&mut *ctx.conn, doc.id).await? {
            let ordered = order_lines
                .get_mut(&line.order_line_id)
                .ok_or_else(|| Error::internal("return line without its order line"))?;
            if ordered.qty_returned - line.qty < ordered.qty_credited {
                return Err(Error::state(format!(
                    "line {}: the customer has been credited for these goods; reverse the credit note first",
                    line.line_no
                ))
                .with_line("lines", line.line_no as usize));
            }
            ordered.qty_delivered += line.qty;
            ordered.qty_returned -= line.qty;
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
            "sales_order.return",
            "Draft a return of goods from the customer of a sales order, open or closed, with a \
             line for everything delivered on it and not yet returned. Nothing is posted: keep \
             the lines and quantities that came back, then post the return with \
             sales_return.post.",
        )
        .label("Return")
        .permission("sales_return:create")
        .from_states(&["open", "closed"])
        .creates(SALES_RETURN)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DocumentView> {
        let order = document::lock(ctx, SALES_ORDER, input.id, "read").await?;
        if !matches!(order.status.as_str(), "open" | "closed") {
            return Err(Error::state(format!(
                "the sales order is {}; goods come back only against an open or closed order",
                order.status
            )));
        }
        let rows: Vec<Value> = order_lines(&mut *ctx.conn, order.id)
            .await?
            .iter()
            .filter(|l| l.qty_delivered > Decimal::ZERO)
            .map(|l| json!({ "order_line_id": l.id, "qty": l.qty_delivered }))
            .collect();
        if rows.is_empty() {
            return Err(Error::validation(
                "nothing has been delivered on this order that has not come back",
            ));
        }
        document::create(
            ctx,
            document::NewDocument {
                doc_type: SALES_RETURN.into(),
                company_id: order.company_id,
                doc_date: today_or(input.doc_date),
                memo: None,
                body: json!({ "sales_order_id": order.id, "lines": rows }),
            },
        )
        .await
    }
}

// -------------------------------------------------------- sales credit note

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreditBody {
    sales_invoice_id: Uuid,
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
    tax_amount: Option<Decimal>,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    #[serde(skip)]
    order_line_id: Option<Uuid>,
    #[serde(skip)]
    invoice_qty: Decimal,
    #[serde(skip)]
    invoice_price: Decimal,
    /// What the invoice line posted, net of its discounts.
    #[serde(skip)]
    invoice_net: Decimal,
    /// The charge or account of the line, on the invoice or its order.
    #[serde(skip)]
    charge_id: Option<Uuid>,
    #[serde(skip)]
    account_id: Option<Uuid>,
}

async fn credit_lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<CreditLine>> {
    let lines = sqlx::query_as(
        "SELECT c.id, c.line_no, c.invoice_line_id, c.qty, c.unit_price, i.tax_code_id,
                c.returned_qty, c.tax_amount, c.discount_percent, c.discount_amount,
                i.order_line_id, i.qty AS invoice_qty, i.unit_price AS invoice_price,
                COALESCE(i.net_amount, round(i.qty * i.unit_price, 2)) AS invoice_net,
                COALESCE(i.charge_id, o.charge_id) AS charge_id,
                COALESCE(i.account_id, o.account_id) AS account_id
         FROM sales_credit_note_line c
         JOIN sales_invoice_line i ON i.id = c.invoice_line_id
         LEFT JOIN sales_order_line o ON o.id = i.order_line_id
         WHERE c.document_id = $1 ORDER BY c.line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(lines)
}

async fn credit_refs(conn: &mut PgConnection, document_id: Uuid) -> Result<(Uuid, Option<Uuid>)> {
    let refs = sqlx::query_as(
        "SELECT sales_invoice_id, sales_order_id FROM sales_credit_note WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(refs)
}

async fn credited_on(
    conn: &mut PgConnection,
    invoice_line_id: Uuid,
    except: Uuid,
) -> Result<Decimal> {
    let credited = sqlx::query_scalar(
        "SELECT COALESCE(sum(c.qty), 0) FROM sales_credit_note_line c
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
        "SELECT COALESCE(sum(c.net_amount), 0) FROM sales_credit_note_line c
         JOIN document d ON d.id = c.document_id
         WHERE c.invoice_line_id = $1 AND d.status = 'posted' AND d.id <> $2",
    )
    .bind(invoice_line_id)
    .bind(except)
    .fetch_one(&mut *conn)
    .await?;
    Ok(credited)
}

/// Whether posted credit notes credit any line of this invoice.
/// The currency of a posted sales invoice and the rate it was posted at:
/// what a credit note for it is in, and posts at.
async fn invoice_currency(
    conn: &mut PgConnection,
    invoice: &Document,
) -> Result<(String, Decimal)> {
    let (currency, rate): (String, Option<Decimal>) = sqlx::query_as(
        "SELECT COALESCE(o.currency, s.currency, c.base_currency), s.fx_rate FROM sales_invoice s
         LEFT JOIN sales_order o ON o.document_id = s.sales_order_id
         JOIN company c ON c.id = $2
         WHERE s.document_id = $1",
    )
    .bind(invoice.id)
    .bind(invoice.company_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok((currency, rate.unwrap_or(Decimal::ONE)))
}

pub(crate) async fn is_credited(conn: &mut PgConnection, invoice_id: Uuid) -> Result<bool> {
    let credited = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM sales_credit_note_line c
            JOIN document d ON d.id = c.document_id
            JOIN sales_invoice_line i ON i.id = c.invoice_line_id
            WHERE i.document_id = $1 AND d.status = 'posted')",
    )
    .bind(invoice_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(credited)
}

pub(crate) struct SalesCreditNote;

#[async_trait]
impl DocumentHandler for SalesCreditNote {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: SALES_CREDIT_NOTE.into(),
            module: String::new(),
            label: "Sales credit note".into(),
            number_prefix: "CN".into(),
            fields: vec![
                FieldMeta::new(
                    "sales_invoice_id",
                    "Sales invoice",
                    FieldType::document_in(SALES_INVOICE, &["posted"]),
                )
                .required()
                .immutable(),
                FieldMeta::new(
                    "sales_order_id",
                    "Sales order",
                    FieldType::document(SALES_ORDER),
                )
                .readonly(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly()
                .help("The invoice's currency."),
                FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
                    .rate(erpcraft_gl::RATE_PLACES)
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
                        FieldType::line_of("sales_invoice_id", SALES_INVOICE, "lines"),
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
                ])
                .collect(),
            }],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
            summary: vec![
                FieldMeta::new(
                    "sales_invoice_id",
                    "Sales invoice",
                    FieldType::document(SALES_INVOICE),
                )
                .readonly(),
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
            erpcraft_kernel::input::parse(body.clone(), "sales credit note body")?;
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
            "SELECT sales_invoice_id FROM sales_credit_note WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        match existing {
            Some(current) if current != body.sales_invoice_id => {
                return Err(Error::validation(
                    "the invoice of a credit note cannot change; create a new credit note instead",
                ));
            }
            Some(_) => {}
            None => {
                let invoice: Option<(String, Option<Uuid>)> = sqlx::query_as(
                    "SELECT d.status, s.sales_order_id FROM document d
                     JOIN sales_invoice s ON s.document_id = d.id
                     WHERE d.id = $1 AND d.company_id = $2",
                )
                .bind(body.sales_invoice_id)
                .bind(doc.company_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
                let order_id = match invoice {
                    None => {
                        return Err(Error::validation(
                            "sales_invoice_id must be a sales invoice of the document's company",
                        )
                        .with_field("sales_invoice_id"));
                    }
                    Some((status, _)) if status != "posted" => {
                        return Err(Error::state(format!(
                            "the sales invoice is {status}; only a posted invoice can be credited"
                        )));
                    }
                    Some((_, order_id)) => order_id,
                };
                sqlx::query(
                    "INSERT INTO sales_credit_note (document_id, sales_invoice_id, sales_order_id)
                     VALUES ($1, $2, $3)",
                )
                .bind(doc.id)
                .bind(body.sales_invoice_id)
                .bind(order_id)
                .execute(&mut *ctx.conn)
                .await?;
                document::link(ctx, body.sales_invoice_id, doc.id, "credit note").await?;
            }
        }

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
                 FROM sales_invoice_line WHERE id = ANY($1) AND document_id = $2",
            )
            .bind(&ids)
            .bind(body.sales_invoice_id)
            .fetch_all(&mut *ctx.conn)
            .await?;
        if invoiced.len() != ids.len() {
            return Err(Error::validation(
                "every line must refer to a line of the sales invoice",
            ));
        }
        let prices: Vec<(Uuid, Decimal)> = invoiced.iter().map(|i| (i.0, i.1)).collect();
        // The invoice line's discounts unless the credit gives its own.
        let decimals = {
            let currency: String = sqlx::query_scalar(
                "SELECT COALESCE(o.currency, s.currency, c.base_currency) FROM sales_invoice s
                 LEFT JOIN sales_order o ON o.document_id = s.sales_order_id
                 JOIN document d ON d.id = s.document_id JOIN company c ON c.id = d.company_id
                 WHERE s.document_id = $1",
            )
            .bind(body.sales_invoice_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
            erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?
        };
        let discounts: Vec<(Option<Decimal>, Option<Decimal>)> = body
            .lines
            .iter()
            .map(|line| {
                let (percent, amount) = match (line.discount_percent, line.discount_amount) {
                    (None, None) => invoiced
                        .iter()
                        .find(|i| i.0 == line.invoice_line_id)
                        .map(|i| (i.3, i.4.map(|a| round_money(a * line.qty / i.2, decimals))))
                        .unwrap_or_default(),
                    given => given,
                };
                (
                    percent.filter(|p| !p.is_zero()),
                    amount.filter(|a| !a.is_zero()),
                )
            })
            .collect();
        for (index, (percent, amount)) in discounts.iter().enumerate() {
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
        }

        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "sales_credit_note_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_credit_note_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        // One statement for all lines.
        if !body.lines.is_empty() {
            let line_nos: Vec<i32> = (1..=body.lines.len() as i32).collect();
            sqlx::query(
                "INSERT INTO sales_credit_note_line (id, document_id, line_no, invoice_line_id, qty,
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
        summary["currency"] = body["currency"].clone();
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
        summary["sales_invoice_id"] = body["sales_invoice_id"].clone();
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
            "SELECT i.id FROM sales_invoice_line i LEFT JOIN sales_order_line o ON o.id = i.order_line_id
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
        let mut body = json!({
            "sales_invoice_id": invoice,
            "sales_order_id": order,
            "currency": currency,
            "fx_rate": rate,
            "lines": credit_lines(conn, doc.id).await?,
        });
        // A draft shows the tax it will take back, as posting works it out.
        if doc.status == "draft" {
            let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
            if let Some(lines) = body["lines"].as_array_mut() {
                for line in lines {
                    use document::body::uuid;
                    if !line["tax_amount"].is_null() || uuid(&line["tax_code_id"]).is_none() {
                        continue;
                    }
                    let Some(invoice_line) = uuid(&line["invoice_line_id"]) else {
                        continue;
                    };
                    let credited = erpcraft_tax::lines::body_line_net(line, decimals);
                    if let Ok(taxes) = erpcraft_tax::credit(
                        &mut *conn,
                        doc.company_id,
                        invoice_line,
                        credited,
                        uuid(&line["id"]),
                        rate,
                    )
                    .await
                    {
                        line["tax_amount"] = json!(-erpcraft_tax::Tax::total(&taxes));
                    }
                }
            }
        }
        Ok(body)
    }
}

pub(crate) struct PostCreditNote;

#[async_trait]
impl Command for PostCreditNote {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_credit_note.post",
            "Post a draft sales credit note. It takes back revenue and the tax charged, and \
             reduces what the customer owes: an open item below zero, settled with the \
             customer's invoices by a payment or applied to one with payment.apply_credit. A \
             credit beyond the goods that came back is for price only and refused unless the \
             company allows it. Tax is taken back at the invoice's code and rate, in this \
             credit note's period. Posting a credit note that is already posted returns it \
             unchanged.",
        )
        .from_states(&["draft"])
        .permission("sales_credit_note:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_CREDIT_NOTE, input.id, "post").await?;
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
        let invoice = document::lock(ctx, SALES_INVOICE, invoice_id, "read").await?;
        if invoice.status != "posted" {
            return Err(Error::state(format!(
                "the sales invoice is {}; only a posted invoice can be credited",
                invoice.status
            )));
        }
        let lines = credit_lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a credit note needs at least one line"));
        }
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        let base_decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        // In the invoice's currency at the invoice's rate: the credit takes
        // back exactly what the invoice booked.
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
        // The share of the invoice the customer kept back as early-payment
        // discount was never paid: of what is credited, that share comes
        // off the discount given rather than being owed back, and tax is
        // taken back only on the rest, as the discount took the tax on it.
        let discount_share = erpcraft_openitems::discount_share(&mut *ctx.conn, invoice.id).await?;
        let discount_account = if discount_share.is_zero() {
            None
        } else {
            Some(
                erpcraft_gl::account_for(&mut *ctx.conn, doc.company_id, "discount_given", &[])
                    .await?,
            )
        };

        let mut owed_back_value = Decimal::ZERO;
        let mut owed_back = Decimal::ZERO;
        let mut taxes = vec![];
        let mut gl = vec![];
        let (mut set_returned, mut set_tax, mut set_net) = (vec![], vec![], vec![]);
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
            // Goods that came back are credited first. A line that is not
            // stock (a charge, a service) has nothing to come back.
            let stock = line
                .order_line_id
                .and_then(|id| order_lines.get_mut(&id))
                .filter(|o| o.item_id.is_some());
            let with_return = match stock {
                None => Decimal::ZERO,
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
                        "{} of the goods on order line {} have come back and not been credited, not {}; a credit without a return is not accepted",
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
                    let with_return = line.qty.min(returned);
                    ordered.qty_invoiced -= with_return;
                    ordered.qty_credited += with_return;
                    with_return
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
            let paid = round_money(credited * (Decimal::ONE - discount_share), decimals);
            let discount_back = credited - paid;
            owed_back += paid;
            owed_back_value += round_money(paid * rate, base_decimals);
            let mut tax_back = Decimal::ZERO;
            if line.tax_code_id.is_some() {
                // Each tax the invoice charged on the line, each back to
                // its own account.
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
                            -tax.value(),
                            Decimal::ZERO,
                            line.id,
                        ));
                        gl.push(in_currency(
                            gl_line(accounts.receivable_account_id, Decimal::ZERO, back, line.id),
                            conversion.as_ref(),
                        ));
                    }
                    taxes.push(tax);
                }
            }
            set_returned.push(Some(with_return));
            set_tax.push(Some(tax_back));
            if !credited.is_zero() {
                let revenue = crate::revenue_account(
                    &mut *ctx.conn,
                    doc.company_id,
                    accounts.revenue_account_id,
                    line.charge_id,
                    line.account_id,
                )
                .await?;
                gl.push(in_currency(
                    gl_line(revenue, credited, Decimal::ZERO, line.id),
                    conversion.as_ref(),
                ));
                if !paid.is_zero() {
                    gl.push(in_currency(
                        gl_line(accounts.receivable_account_id, Decimal::ZERO, paid, line.id),
                        conversion.as_ref(),
                    ));
                }
                if let Some(account) = discount_account.filter(|_| !discount_back.is_zero()) {
                    gl.push(in_currency(
                        gl_line(account, Decimal::ZERO, discount_back, line.id),
                        conversion.as_ref(),
                    ));
                }
            }
        }

        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "sales_credit_note_line",
            &ids,
            &[
                ("returned_qty", &set_returned),
                ("tax_amount", &set_tax),
                ("net_amount", &set_net),
            ],
        )
        .await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        // The last of an invoice's tax in a foreign currency takes the last
        // of its accounting value, which can differ by a cent from what its
        // amount is worth at the rate.
        if let Some(rounding) =
            erpcraft_gl::exchange_difference(&mut *ctx.conn, doc.company_id, &gl, None).await?
        {
            gl.push(rounding);
        }
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
            let scopes = crate::gl_scopes(&mut *ctx.conn, &about).await?;
            let customer = crate::invoice_customer(&mut *ctx.conn, invoice_id).await?;
            erpcraft_gl::add_scopes(&mut gl, &scopes, &[("customer", customer)]);
            erpcraft_gl::post(ctx, &posting, &gl).await?;
        }
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        if conversion.is_some() {
            sqlx::query("UPDATE sales_credit_note SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }
        if !owed_back.is_zero() {
            // The party of the invoice: its order's, or its own.
            let party_id: Uuid = sqlx::query_scalar(
                "SELECT COALESCE(i.customer_id, o.customer_id) FROM sales_invoice i
                 LEFT JOIN sales_order o ON o.document_id = i.sales_order_id
                 WHERE i.document_id = $1",
            )
            .bind(invoice_id)
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
        // The parties as the invoice showed them, kept from now on.
        let customer = crate::invoice_customer(&mut *ctx.conn, invoice_id).await?;
        crate::keep_parties(&mut *ctx.conn, &doc, invoice_id, customer).await?;
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
            "sales_credit_note.reverse",
            "Cancel a posted sales credit note entered by mistake. What it credited is owed \
             again, and the returned goods it covered wait for a credit again. Refused once the \
             credit has been settled or applied; reverse that payment first.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("sales_credit_note:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_CREDIT_NOTE, input.id, "reverse").await?;
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
            ordered.qty_invoiced += with_return;
            ordered.qty_credited -= with_return;
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
            "sales_invoice.credit",
            "Draft a credit note for a posted sales invoice, with a line for each of its lines \
             whose goods came back and are not yet credited, at the invoice's price. Nothing is \
             posted: set the quantities and prices to credit, then post it with \
             sales_credit_note.post.",
        )
        .label("Credit")
        .permission("sales_credit_note:create")
        .from_states(&["posted"])
        .creates(SALES_CREDIT_NOTE)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DraftInput) -> Result<DocumentView> {
        let invoice = document::lock(ctx, SALES_INVOICE, input.id, "read").await?;
        if invoice.status != "posted" {
            return Err(Error::state(format!(
                "the sales invoice is {}; only a posted invoice can be credited",
                invoice.status
            )));
        }
        let order_id: Option<Uuid> =
            sqlx::query_scalar("SELECT sales_order_id FROM sales_invoice WHERE document_id = $1")
                .bind(invoice.id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        // A direct invoice has no order, and nothing came back on one.
        let order = match order_id {
            Some(order_id) => order_lines(&mut *ctx.conn, order_id).await?,
            None => vec![],
        };
        // Lines that are not on the order (charges) have nothing to come
        // back: they are credited line by line.
        let invoice_lines: Vec<(Uuid, Uuid, Decimal)> = sqlx::query_as(
            "SELECT id, order_line_id, qty FROM sales_invoice_line
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
                "nothing that came back on this invoice's lines is waiting for a credit; a credit \
                 for price only is entered line by line",
            ));
        }
        document::create(
            ctx,
            document::NewDocument {
                doc_type: SALES_CREDIT_NOTE.into(),
                company_id: invoice.company_id,
                doc_date: today_or(input.doc_date),
                memo: None,
                body: json!({ "sales_invoice_id": invoice.id, "lines": rows }),
            },
        )
        .await
    }
}

// ------------------------------------------------------- awaiting a credit

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AwaitingInput {
    /// The company whose orders are looked at.
    company_id: Uuid,
    /// Only this customer.
    customer_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct AwaitingRow {
    /// The order the goods were delivered on.
    order_id: Uuid,
    order_number: Option<String>,
    customer_id: Uuid,
    customer_name: String,
    line_no: i32,
    item_id: Option<Uuid>,
    item_code: Option<String>,
    /// Returned and invoiced, and not yet credited.
    qty: Decimal,
    /// The order line's price, in the order's currency.
    unit_price: Decimal,
    currency: String,
    /// The quantity at the order line's price, before discounts and tax.
    amount: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AwaitingOutput {
    company_id: Uuid,
    rows: Vec<AwaitingRow>,
}

pub(crate) struct AwaitingCredit;

#[async_trait]
impl Command for AwaitingCredit {
    type Input = AwaitingInput;
    type Output = AwaitingOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "sales_order.awaiting_credit",
            "Goods customers sent back that they had been invoiced for and have not yet been credited, per sales order line of a company: what a sales credit note is still owed for, at the order line's price.",
        )
        .report("Returns to credit")
        .permission("sales_order:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: AwaitingInput) -> Result<AwaitingOutput> {
        ctx.require("sales_order:read", input.company_id)?;
        let items = ctx.actor.can_any("item:read");
        // Of what was returned, what had been invoiced and is not credited:
        // net invoiced beyond net delivered, no more than returned less
        // credited.
        let rows: Vec<AwaitingRow> = sqlx::query_as(
            "WITH open AS (
                SELECT l.document_id, l.line_no, l.item_id, l.unit_price,
                       LEAST(p.qty_returned - p.qty_credited,
                             (p.qty_invoiced - p.qty_credited)
                               - (p.qty_delivered - p.qty_returned)) AS qty
                FROM sales_order_line l JOIN sales_order_progress p ON p.line_id = l.id)
             SELECT d.id AS order_id, d.number AS order_number, x.id AS customer_id,
                    x.name AS customer_name, o.line_no, o.item_id,
                    CASE WHEN $3 THEN i.code END AS item_code, trim_scale(o.qty) AS qty,
                    trim_scale(o.unit_price) AS unit_price,
                    COALESCE(h.currency, c.base_currency) AS currency,
                    round(o.qty * o.unit_price, cur.decimals) AS amount
             FROM open o
             JOIN document d ON d.id = o.document_id
             JOIN sales_order h ON h.document_id = d.id
             JOIN customer x ON x.id = h.customer_id
             JOIN company c ON c.id = d.company_id
             JOIN currency cur ON cur.code = COALESCE(h.currency, c.base_currency)
             LEFT JOIN item i ON i.id = o.item_id
             WHERE d.company_id = $1 AND o.qty > 0
               AND ($2::uuid IS NULL OR h.customer_id = $2)
             ORDER BY x.name, d.number, o.line_no",
        )
        .bind(input.company_id)
        .bind(input.customer_id)
        .bind(items)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(AwaitingOutput {
            company_id: input.company_id,
            rows,
        })
    }
}
