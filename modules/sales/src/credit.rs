//! A credit to a customer that is not tied to an invoice: a rebate over a
//! year's invoices, goodwill, a price agreed after the fact.
//!
//! Its lines are charges or accounts, each with a quantity, a price and a
//! tax code, like the lines of an invoice without an order. Posting takes
//! back revenue (the line's account, or its charge's revenue, else the
//! sales revenue account) and the tax at each line's code, and leaves the
//! customer a credit: an open item below zero, settled against its
//! invoices by a payment or applied to one with `payment.apply_credit`.
//! With no invoice entry to credit, the tax ledger records the tax taken
//! back as entries of their own, below zero (`erpcraft_tax::as_credit`).
//! The company's policy `sales_credit.without_invoice` is asked first,
//! so a company can have such credits approved.

use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    Alternatives, CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error,
    FieldMeta, FieldType, PgConnection, Result, StateMeta, async_trait, company, round_money,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use erpcraft_tax::Direction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    DocInput, ReverseInput, accounting_fields, accounting_totals, add_totals, gl_line, in_currency,
    policy, post_and_reverse, revenue_account, settings, states,
};

pub(crate) const SALES_CREDIT: &str = "sales_credit";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    customer_id: Uuid,
    /// The accounting currency when omitted.
    currency: Option<String>,
    /// For a credit in a foreign currency: the rate to state. The rate on
    /// record for the credit's date when omitted.
    fx_rate: Option<Decimal>,
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    /// The id of the stored line this one is, when it was read before.
    id: Option<Uuid>,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    /// What the tax rules propose when omitted.
    tax_code_id: Option<Uuid>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    tax_code_id: Option<Uuid>,
    net_amount: Option<Decimal>,
    tax_amount: Option<Decimal>,
}

async fn lines(conn: &mut PgConnection, document_id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, charge_id, account_id, description, qty, unit_price, tax_code_id,
                net_amount, tax_amount
         FROM sales_credit_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The customer, the currency and the rate stated or posted at.
async fn header(
    conn: &mut PgConnection,
    doc: &Document,
) -> Result<(Uuid, String, Option<Decimal>)> {
    Ok(sqlx::query_as(
        "SELECT s.customer_id, COALESCE(s.currency, c.base_currency), s.fx_rate
         FROM sales_credit s JOIN company c ON c.id = $2 WHERE s.document_id = $1",
    )
    .bind(doc.id)
    .bind(doc.company_id)
    .fetch_one(&mut *conn)
    .await?)
}

pub(crate) struct SalesCredit;

#[async_trait]
impl DocumentHandler for SalesCredit {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: SALES_CREDIT.into(),
            module: String::new(),
            label: "Customer credit".into(),
            number_prefix: "CR".into(),
            fields: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).required(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .help("The accounting currency when left out."),
                FieldMeta::new("fx_rate", "Exchange rate", FieldType::Decimal)
                    .rate(10)
                    .help(
                        "For a credit in another currency: accounting currency per unit. The \
                         rate on record for the document's date when left out; once posted, \
                         the rate it was posted at.",
                    ),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![FieldMeta::line_id(), FieldMeta::line_no()]
                    .into_iter()
                    .chain(erpcraft_tax::lines::line_kind_fields(
                        "For a line that is not a charge: the account it takes back from, such \
                         as revenue or a rebates account.",
                    ))
                    .chain([
                        FieldMeta::new("qty", "Quantity", FieldType::Decimal)
                            .quantity()
                            .required(),
                        FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                            .unit_price()
                            .currency_of_document("currency")
                            .required(),
                        FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                            .help("What the tax rules propose when left out."),
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
            alternatives: vec![Alternatives::new(
                "lines",
                &[("Charge", &["charge_id"]), ("Account", &["account_id"])],
            )],
            states: states(StateMeta::editable("draft", "Draft")),
            initial_state: "draft".into(),
            transitions: post_and_reverse(),
            summary: vec![
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
                    .readonly()
                    .help("What the customer is credited, tax included."),
            ]
            .into_iter()
            .chain(accounting_fields())
            .collect(),
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let mut body: Body = erpcraft_kernel::input::parse(body.clone(), "customer credit body")?;
        for (index, line) in body.lines.iter().enumerate() {
            erpcraft_tax::lines::check_line(
                ctx,
                doc.company_id,
                index + 1,
                false,
                line.charge_id,
                line.account_id,
                line.qty,
                line.unit_price,
                None,
                None,
            )
            .await?;
        }
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customer WHERE id = $1)")
                .bind(body.customer_id)
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
        if body
            .fx_rate
            .is_some_and(|rate| rate <= Decimal::ZERO || rate.normalize().scale() > 10)
        {
            return Err(Error::validation(
                "an exchange rate is more than zero, with at most 10 decimal places",
            )
            .with_field("fx_rate"));
        }
        // What the tax rules propose for a line that names no code.
        for line in &mut body.lines {
            if line.tax_code_id.is_none() {
                line.tax_code_id = erpcraft_tax::proposed_code(
                    &mut *ctx.conn,
                    doc.company_id,
                    ("customer", body.customer_id),
                    None,
                    line.charge_id,
                )
                .await?;
            }
        }
        sqlx::query(
            "INSERT INTO sales_credit (document_id, customer_id, currency, fx_rate)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (document_id) DO UPDATE SET customer_id = EXCLUDED.customer_id,
                currency = EXCLUDED.currency, fx_rate = EXCLUDED.fx_rate",
        )
        .bind(doc.id)
        .bind(body.customer_id)
        .bind(currency)
        .bind(body.fx_rate)
        .execute(&mut *ctx.conn)
        .await?;
        let ids = document::line_ids(
            &mut *ctx.conn,
            "sales_credit_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_credit_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (index, (line, id)) in body.lines.iter().zip(&ids).enumerate() {
            sqlx::query(
                "INSERT INTO sales_credit_line (id, document_id, line_no, charge_id, account_id,
                     description, qty, unit_price, tax_code_id)
                 VALUES ($1, $2, $3, $4, $5, NULLIF(trim($6), ''), $7, $8, $9)",
            )
            .bind(id)
            .bind(doc.id)
            .bind(index as i32 + 1)
            .bind(line.charge_id)
            .bind(line.account_id)
            .bind(&line.description)
            .bind(line.qty)
            .bind(line.unit_price)
            .bind(line.tax_code_id)
            .execute(&mut *ctx.conn)
            .await?;
        }
        Ok(())
    }

    /// The customer and what the credit comes to: net, the tax (as it
    /// will be worked out while a draft, as posted after), and the total.
    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        use document::body::{decimal, lines, uuid};
        let customer = uuid(&body["customer_id"]);
        let name: Option<String> = sqlx::query_scalar("SELECT name FROM customer WHERE id = $1")
            .bind(customer)
            .fetch_optional(&mut *conn)
            .await?;
        let currency = match body["currency"].as_str() {
            Some(currency) => currency.to_owned(),
            None => company::base_currency(conn, doc.company_id).await?,
        };
        let decimals = erpcraft_gl::currency_decimals(conn, &currency).await?;
        let base = company::base_currency(conn, doc.company_id).await?;
        let mut net = Decimal::ZERO;
        let mut tax = Decimal::ZERO;
        for line in lines(body, "lines") {
            let line_net = decimal(&line["net_amount"]).unwrap_or_else(|| {
                let qty = decimal(&line["qty"]).unwrap_or_default();
                let price = decimal(&line["unit_price"]).unwrap_or_default();
                round_money(qty * price, decimals)
            });
            net += line_net;
            tax += match (uuid(&line["tax_code_id"]), decimal(&line["tax_amount"])) {
                (_, Some(posted)) => posted,
                (Some(code), None) => erpcraft_tax::estimate(
                    conn,
                    doc.company_id,
                    code,
                    Direction::Collected,
                    line_net,
                    None,
                    None,
                )
                .await
                .map(|taxes| erpcraft_tax::Tax::total(&taxes))
                .unwrap_or_default(),
                (None, None) => Decimal::ZERO,
            };
        }
        let mut summary = json!({
            "customer_id": customer,
            "customer_name": name,
            "currency": currency,
            "total_net": net,
            "total_tax": tax,
            "total_gross": net + tax,
        });
        let totals = accounting_totals(conn, doc, currency != base, net, Some(tax)).await?;
        add_totals(&mut summary, totals);
        Ok(summary)
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let (customer, currency, fx_rate) = header(conn, doc).await?;
        let mut body = json!({
            "customer_id": customer,
            "currency": currency,
            "fx_rate": fx_rate,
            "lines": lines(conn, doc.id).await?,
        });
        crate::fill_draft_lines(conn, doc, &mut body, Some(Direction::Collected)).await?;
        Ok(body)
    }
}

pub(crate) struct PostCredit;

#[async_trait]
impl Command for PostCredit {
    type Input = DocInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_credit.post",
            "Post a draft customer credit, a credit not tied to an invoice (a rebate, goodwill). \
             It takes back revenue, or charges the line's account, and the tax at each line's \
             code, in this credit's period, and leaves the customer a credit: an open item below \
             zero, settled with its invoices by a payment or applied to one with \
             payment.apply_credit. The company's policy sales_credit.without_invoice is asked \
             first. Posting a credit that is already posted returns it unchanged.",
        )
        .from_states(&["draft"])
        .permission("sales_credit:post")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DocInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_CREDIT, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} customer credit cannot be posted",
                doc.status
            )));
        }
        let lines = lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation(
                "a customer credit needs at least one line",
            ));
        }
        let (customer, currency, stated) = header(&mut *ctx.conn, &doc).await?;
        let (code, active): (String, bool) =
            sqlx::query_as("SELECT code, active FROM customer WHERE id = $1")
                .bind(customer)
                .fetch_one(&mut *ctx.conn)
                .await?;
        ctx.decide(
            policy::INACTIVE_CUSTOMER,
            doc.company_id,
            policy::inactive_customer(SALES_CREDIT, &code, active),
        )
        .await?;
        let accounts = settings(&mut *ctx.conn, doc.company_id).await?;
        let rate = erpcraft_gl::document_rate(
            ctx,
            doc.company_id,
            &currency,
            doc.doc_date,
            stated,
            SALES_CREDIT,
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

        let mut owed_back = Decimal::ZERO;
        let mut owed_back_value = Decimal::ZERO;
        let mut net_amounts = vec![];
        let mut tax_amounts = vec![];
        let mut taxes = vec![];
        let mut gl = vec![];
        for line in &lines {
            let net = round_money(line.qty * line.unit_price, decimals);
            let account = revenue_account(
                &mut *ctx.conn,
                doc.company_id,
                accounts.revenue_account_id,
                line.charge_id,
                line.account_id,
            )
            .await?;
            net_amounts.push(Some(net));
            owed_back += net;
            owed_back_value += round_money(net * rate, base_decimals);
            if !net.is_zero() {
                gl.push(in_currency(
                    gl_line(account, net, Decimal::ZERO, line.id),
                    conversion.as_ref(),
                ));
                gl.push(in_currency(
                    gl_line(accounts.receivable_account_id, Decimal::ZERO, net, line.id),
                    conversion.as_ref(),
                ));
            }
            // The tax on it, taken back from the account it is owed on.
            let Some(code) = line.tax_code_id else {
                tax_amounts.push(None);
                continue;
            };
            let line_taxes = erpcraft_tax::as_credit(
                erpcraft_tax::quote(
                    ctx,
                    doc.company_id,
                    code,
                    Direction::Collected,
                    net,
                    Some(line.id),
                    conversion.as_ref(),
                )
                .await?,
            );
            tax_amounts.push(Some(-erpcraft_tax::Tax::total(&line_taxes)));
            for tax in line_taxes {
                let back = -tax.amount;
                if !back.is_zero() && !tax.self_assessed {
                    owed_back += back;
                    owed_back_value += -tax.value();
                    let (debit, credit) = tax.debit_credit();
                    gl.push(gl_line(tax.account_id, debit, credit, line.id));
                    gl.push(in_currency(
                        gl_line(accounts.receivable_account_id, Decimal::ZERO, back, line.id),
                        conversion.as_ref(),
                    ));
                }
                taxes.push(tax);
            }
        }
        // Asked with what it comes to, so a company can have large
        // credits approved.
        ctx.decide(
            policy::WITHOUT_INVOICE,
            doc.company_id,
            policy::without_invoice(owed_back, &currency, &code),
        )
        .await?;

        let ids: Vec<Uuid> = lines.iter().map(|l| l.id).collect();
        document::set_line_values(
            &mut *ctx.conn,
            "sales_credit_line",
            &ids,
            &[("net_amount", &net_amounts), ("tax_amount", &tax_amounts)],
        )
        .await?;
        let posting = document::create_posting(ctx, &doc, doc.doc_date, doc.memo.clone()).await?;
        if let Some(rounding) =
            erpcraft_gl::exchange_difference(&mut *ctx.conn, doc.company_id, &gl, None).await?
        {
            gl.push(rounding);
        }
        if !gl.is_empty() {
            erpcraft_gl::post(&mut *ctx.conn, &posting, &gl).await?;
        }
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        if !owed_back.is_zero() {
            erpcraft_openitems::open(
                &mut *ctx.conn,
                &posting,
                &NewOpenItem {
                    kind: Kind::Receivable,
                    party_type: "customer",
                    party_id: customer,
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
        if conversion.is_some() {
            sqlx::query("UPDATE sales_credit SET fx_rate = $2 WHERE document_id = $1")
                .bind(doc.id)
                .bind(rate)
                .execute(&mut *ctx.conn)
                .await?;
        }
        erpcraft_party::snapshot_sale(&mut *ctx.conn, doc.id, doc.company_id, customer, None, None)
            .await?;
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

pub(crate) struct ReverseCredit;

#[async_trait]
impl Command for ReverseCredit {
    type Input = ReverseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_credit.reverse",
            "Cancel a posted customer credit entered by mistake: revenue, tax and what the \
             customer is owed back are as they were. Refused once the credit has been settled \
             or applied; reverse that payment first.",
        )
        .from_states(&["posted"])
        .destructive()
        .permission("sales_credit:reverse")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReverseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, SALES_CREDIT, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} customer credit cannot be reversed",
                doc.status
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
