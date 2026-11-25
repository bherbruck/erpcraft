//! Sales quotes: prices offered to a customer, proposed by the price
//! lists, issued, and won into a sales order or lost, with the reason.
//! See "Sales quotes" in ARCHITECTURE.md.

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, PgConnection, PolicyPoint, Result, StateMeta, Utc,
    async_trait, command, company, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const QUOTE: &str = "sales_quote";
/// Asked when a quote is accepted after its date.
pub const EXPIRED: &str = "sales_quote.expired";

pub struct QuotesModule;

impl Module for QuotesModule {
    fn name(&self) -> &'static str {
        "quotes"
    }

    fn label(&self) -> String {
        "Quotes".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["sales", "pricing", "tax"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "quotes",
            sql: include_str!("../migrations/0001_quotes.sql"),
        }]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                EXPIRED,
                "Asked when a sales quote is accepted after its valid-until date: may it still \
                 become an order at its prices? Unless configured otherwise it may not; a \
                 company allows a few days, for example `facts.days_expired <= 7`.",
            )
            .number("days_expired", "Days since the quote's valid-until date")
            .number("total", "The quote's net total")
            .as_money(Some("currency"))
            .text("currency", "The quote's currency")
            .refuse_when("facts.days_expired > 0")
            .refusal("the quote has expired"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(Quote)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Issue), command(Accept), command(Lose)]
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    customer_id: Uuid,
    currency: Option<String>,
    valid_until: NaiveDate,
    #[serde(default)]
    lines: Vec<LineInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    id: Option<Uuid>,
    item_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    /// Left out on an item line: what the price lists propose.
    unit_price: Option<Decimal>,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    item_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    charge_id: Option<Uuid>,
    account_id: Option<Uuid>,
    description: Option<String>,
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
}

async fn lines(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, item_id, warehouse_id, charge_id, account_id, description, qty,
                unit_price, discount_percent, discount_amount
         FROM sales_quote_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The customer, currency (the accounting one when none) and date.
async fn header(conn: &mut PgConnection, doc: &Document) -> Result<(Uuid, String, NaiveDate)> {
    let (customer, currency, valid_until): (Uuid, Option<String>, NaiveDate) = sqlx::query_as(
        "SELECT customer_id, currency, valid_until FROM sales_quote WHERE document_id = $1",
    )
    .bind(doc.id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::validation("the quote names no customer yet"))?;
    let currency = match currency {
        Some(c) => c,
        None => company::base_currency(&mut *conn, doc.company_id).await?,
    };
    Ok((customer, currency, valid_until))
}

struct Quote;

#[async_trait]
impl DocumentHandler for Quote {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: QUOTE.into(),
            module: String::new(),
            label: "Sales quote".into(),
            plural: "Sales quotes".into(),
            number_prefix: "SQ".into(),
            fields: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).required(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .help("The accounting currency when empty."),
                FieldMeta::new("valid_until", "Valid until", FieldType::Date)
                    .required()
                    .help("After it, accepting asks the policy sales_quote.expired."),
                FieldMeta::new("outcome", "Outcome", FieldType::Text).readonly(),
                FieldMeta::new("reason", "Reason", FieldType::Text)
                    .readonly()
                    .help("Why it was won or lost."),
                FieldMeta::new(
                    "order_id",
                    "Sales order",
                    FieldType::document("sales_order"),
                )
                .readonly(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Lines".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new("item_id", "Item", FieldType::link("item"))
                        .help("For a stock line, with the warehouse it would leave from."),
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
                        .help("Left out on an item line: what the price lists propose."),
                ])
                .chain(erpcraft_tax::lines::discount_fields())
                .collect(),
            }],
            alternatives: vec![erpcraft_tax::lines::line_kinds("lines")],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("issued", "Issued"),
                StateMeta::locked("won", "Won"),
                StateMeta::locked("lost", "Lost"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "issued".into()),
                ("issued".into(), "won".into()),
                ("issued".into(), "lost".into()),
                ("draft".into(), "lost".into()),
            ],
            summary: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).readonly(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly(),
                FieldMeta::new("total_net", "Total", FieldType::Decimal)
                    .money()
                    .currency_in("currency")
                    .readonly()
                    .help("The lines at their prices, without tax."),
                FieldMeta::new("valid_until", "Valid until", FieldType::Date).readonly(),
                FieldMeta::new("expired", "Expired", FieldType::Bool)
                    .readonly()
                    .help("Its valid-until date has passed."),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let mut body: Body = erpcraft_kernel::input::parse(body.clone(), "sales quote body")?;
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM customer WHERE id = $1)")
                .bind(body.customer_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        if !known {
            return Err(Error::validation("the customer does not exist").with_field("customer_id"));
        }
        if body.valid_until < doc.doc_date {
            return Err(
                Error::validation("a quote is valid at least until its own date")
                    .with_field("valid_until"),
            );
        }
        let base = company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let currency = body.currency.clone().filter(|c| *c != base);
        if let Some(c) = &currency {
            erpcraft_gl::currency_decimals(&mut *ctx.conn, c)
                .await
                .map_err(|e| e.with_field("currency"))?;
        }
        let quote_currency = currency.clone().unwrap_or_else(|| base.clone());
        let mut proposals = vec![];
        for (index, line) in body.lines.iter_mut().enumerate() {
            let n = index + 1;
            if line.item_id.is_some() != line.warehouse_id.is_some() {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("warehouse_id"),
                    "a stock line names both its item and its warehouse",
                ));
            }
            let proposal = match line.item_id {
                Some(item) => {
                    erpcraft_pricing::propose(
                        &mut *ctx.conn,
                        &erpcraft_pricing::Query {
                            company_id: doc.company_id,
                            side: erpcraft_pricing::Side::Sales,
                            party_id: body.customer_id,
                            item_id: item,
                            qty: line.qty,
                            currency: quote_currency.clone(),
                            date: doc.doc_date,
                        },
                    )
                    .await?
                }
                None => None,
            };
            if line.unit_price.is_none() {
                line.unit_price = proposal.as_ref().and_then(|p| p.unit_price);
                if line.unit_price.is_some() && line.discount_percent.is_none() {
                    line.discount_percent = proposal.as_ref().and_then(|p| p.discount_percent);
                }
            }
            let Some(price) = line.unit_price else {
                return Err(Error::line(
                    "lines",
                    n,
                    Some("unit_price"),
                    "give a unit price: no price list has one for this line",
                ));
            };
            erpcraft_tax::lines::check_line(
                ctx,
                doc.company_id,
                n,
                line.item_id.is_some(),
                line.charge_id,
                line.account_id,
                line.qty,
                price,
                line.discount_percent,
                line.discount_amount,
            )
            .await?;
            line.discount_percent = erpcraft_tax::lines::no_zero(line.discount_percent);
            line.discount_amount = erpcraft_tax::lines::no_zero(line.discount_amount);
            proposals.push(proposal);
        }
        let warehouses: Vec<Uuid> = body.lines.iter().filter_map(|l| l.warehouse_id).collect();
        if !company::owns_all(&mut *ctx.conn, "warehouse", &warehouses, doc.company_id).await? {
            return Err(Error::validation(
                "every stock line needs a warehouse of the quote's company",
            ));
        }
        sqlx::query(
            "INSERT INTO sales_quote (document_id, customer_id, currency, valid_until)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (document_id) DO UPDATE SET customer_id = EXCLUDED.customer_id,
                 currency = EXCLUDED.currency, valid_until = EXCLUDED.valid_until",
        )
        .bind(doc.id)
        .bind(body.customer_id)
        .bind(&currency)
        .bind(body.valid_until)
        .execute(&mut *ctx.conn)
        .await?;
        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "sales_quote_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM sales_quote_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (n, (line, id)) in body.lines.iter().zip(&line_ids).enumerate() {
            sqlx::query(
                "INSERT INTO sales_quote_line (id, document_id, line_no, item_id, warehouse_id,
                     charge_id, account_id, description, qty, unit_price, discount_percent,
                     discount_amount)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, NULLIF(trim($8), ''), $9, $10, $11, $12)",
            )
            .bind(id)
            .bind(doc.id)
            .bind(n as i32 + 1)
            .bind(line.item_id)
            .bind(line.warehouse_id)
            .bind(line.charge_id)
            .bind(line.account_id)
            .bind(&line.description)
            .bind(line.qty)
            .bind(line.unit_price)
            .bind(line.discount_percent)
            .bind(line.discount_amount)
            .execute(&mut *ctx.conn)
            .await?;
        }
        let kept: Vec<(Uuid, erpcraft_pricing::Proposal)> = line_ids
            .iter()
            .zip(proposals)
            .filter_map(|(id, p)| p.map(|p| (*id, p)))
            .collect();
        erpcraft_pricing::keep(&mut *ctx.conn, doc.id, &kept).await?;
        Ok(())
    }

    async fn summary_of(
        &self,
        conn: &mut PgConnection,
        doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let currency = match body["currency"].as_str() {
            Some(c) => c.to_owned(),
            None => company::base_currency(&mut *conn, doc.company_id).await?,
        };
        let decimals = erpcraft_gl::currency_decimals(&mut *conn, &currency).await?;
        let total: Decimal = document::body::lines(body, "lines")
            .iter()
            .map(|l| erpcraft_tax::lines::body_line_net(l, decimals))
            .sum();
        let expired = body["valid_until"]
            .as_str()
            .and_then(|d| d.parse::<NaiveDate>().ok())
            .is_some_and(|d| d < Utc::now().date_naive());
        Ok(json!({
            "customer_id": body["customer_id"],
            "currency": currency,
            "total_net": total,
            "valid_until": body["valid_until"],
            "expired": expired && doc.status == "issued",
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header: Option<(Uuid, Option<String>, NaiveDate)> = sqlx::query_as(
            "SELECT customer_id, currency, valid_until FROM sales_quote WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *conn)
        .await?;
        let outcome: Option<(String, Option<String>, Option<Uuid>)> = sqlx::query_as(
            "SELECT outcome, reason, order_id FROM sales_quote_outcome WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *conn)
        .await?;
        Ok(json!({
            "customer_id": header.as_ref().map(|h| h.0),
            "currency": header.as_ref().and_then(|h| h.1.clone()),
            "valid_until": header.as_ref().map(|h| h.2),
            "outcome": outcome.as_ref().map(|o| o.0.clone()),
            "reason": outcome.as_ref().and_then(|o| o.1.clone()),
            "order_id": outcome.as_ref().and_then(|o| o.2),
            "lines": lines(conn, doc.id).await?,
        }))
    }
}

// ------------------------------------------------------------ commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdInput {
    /// Sales quote id.
    id: Uuid,
}

struct Issue;

#[async_trait]
impl Command for Issue {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_quote.issue",
            "Issue a reviewed draft quote: it is numbered and locked, ready to print and send \
             with document.send. Each line whose price or discount is not what the price lists \
             proposed asks the policy price.override.",
        )
        .permission("sales_quote:post")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, QUOTE, input.id, "post").await?;
        if doc.status == "issued" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} quote cannot be issued",
                doc.status
            )));
        }
        let lines = lines(&mut *ctx.conn, doc.id).await?;
        if lines.is_empty() {
            return Err(Error::validation("a quote needs at least one line"));
        }
        let mut prices = vec![];
        for line in &lines {
            let Some(item) = line.item_id else { continue };
            let code: String = sqlx::query_scalar("SELECT code FROM item WHERE id = $1")
                .bind(item)
                .fetch_one(&mut *ctx.conn)
                .await?;
            prices.push(erpcraft_pricing::LinePrice {
                line_id: line.id,
                line_no: line.line_no,
                item_code: code,
                unit_price: line.unit_price,
                discount_percent: line.discount_percent,
            });
        }
        erpcraft_pricing::check(
            ctx,
            doc.company_id,
            doc.id,
            erpcraft_pricing::Side::Sales,
            &prices,
        )
        .await?;
        document::transition(ctx, &mut doc, "issued").await?;
        document::view(ctx, doc).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AcceptInput {
    /// Sales quote id.
    id: Uuid,
    /// Why the customer took it.
    reason: Option<String>,
    /// The order's date; today when omitted.
    order_date: Option<NaiveDate>,
}

struct Accept;

#[async_trait]
impl Command for Accept {
    type Input = AcceptInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_quote.accept",
            "The customer accepted an issued quote: draft a sales order with its lines, prices \
             and discounts, linked to it, and mark the quote won (with a reason, if given). \
             After the quote's valid-until date the policy sales_quote.expired is asked \
             (refused by default). Returns the draft order, to approve as any other.",
        )
        .permission("sales_quote:post")
        .from_states(&["issued"])
        .creates("sales_order")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: AcceptInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, QUOTE, input.id, "post").await?;
        if doc.status != "issued" {
            return Err(Error::state(format!(
                "a {} quote cannot be accepted",
                doc.status
            )));
        }
        ctx.require("sales_order:create", doc.company_id)?;
        let (customer, currency, valid_until) = header(&mut *ctx.conn, &doc).await?;
        let date = input.order_date.unwrap_or_else(|| Utc::now().date_naive());
        let lines = lines(&mut *ctx.conn, doc.id).await?;
        let days_expired = (date - valid_until).num_days();
        if days_expired > 0 {
            let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &currency).await?;
            let total: Decimal = lines
                .iter()
                .filter_map(|l| {
                    erpcraft_tax::line_net(
                        l.qty,
                        l.unit_price,
                        l.discount_percent,
                        l.discount_amount,
                        decimals,
                    )
                })
                .sum();
            ctx.decide(
                EXPIRED,
                doc.company_id,
                json!({ "days_expired": days_expired, "total": total.to_string(),
                        "currency": currency }),
            )
            .await?;
        }
        let rows: Vec<Value> = lines
            .iter()
            .map(|l| {
                json!({
                    "item_id": l.item_id, "warehouse_id": l.warehouse_id,
                    "charge_id": l.charge_id, "account_id": l.account_id,
                    "description": l.description, "qty": l.qty, "unit_price": l.unit_price,
                    "discount_percent": l.discount_percent, "discount_amount": l.discount_amount,
                })
            })
            .collect();
        let order = document::create(
            ctx,
            document::NewDocument {
                doc_type: "sales_order".into(),
                company_id: doc.company_id,
                doc_date: date,
                memo: doc.number.as_ref().map(|n| format!("Quote {n}")),
                body: json!({ "customer_id": customer, "currency": currency, "lines": rows }),
            },
        )
        .await?;
        document::link(ctx, doc.id, order.header.id, "sales_order").await?;
        sqlx::query(
            "INSERT INTO sales_quote_outcome (document_id, outcome, reason, order_id)
             VALUES ($1, 'won', NULLIF(trim($2), ''), $3)",
        )
        .bind(doc.id)
        .bind(&input.reason)
        .bind(order.header.id)
        .execute(&mut *ctx.conn)
        .await?;
        document::transition(ctx, &mut doc, "won").await?;
        Ok(order)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LoseInput {
    /// Sales quote id.
    id: Uuid,
    /// Why it was lost: the price, a competitor, no decision.
    reason: String,
}

struct Lose;

#[async_trait]
impl Command for Lose {
    type Input = LoseInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sales_quote.lose",
            "Mark a quote lost, with the reason (the price, a competitor, no decision). It does \
             not change again.",
        )
        .permission("sales_quote:post")
        .from_states(&["draft", "issued"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: LoseInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, QUOTE, input.id, "post").await?;
        if !["draft", "issued"].contains(&doc.status.as_str()) {
            return Err(Error::state(format!(
                "a {} quote cannot be lost",
                doc.status
            )));
        }
        if input.reason.trim().is_empty() {
            return Err(Error::validation("say why the quote was lost").with_field("reason"));
        }
        sqlx::query(
            "INSERT INTO sales_quote_outcome (document_id, outcome, reason) VALUES ($1, 'lost', $2)",
        )
        .bind(doc.id)
        .bind(input.reason.trim())
        .execute(&mut *ctx.conn)
        .await?;
        document::transition(ctx, &mut doc, "lost").await?;
        document::view(ctx, doc).await
    }
}
