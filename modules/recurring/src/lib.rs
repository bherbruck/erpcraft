//! Recurring invoices: templates of a customer's invoice with a schedule,
//! and a run that makes the invoices due, as drafts or posted as the
//! company's policy allows. See "Recurring invoices" in ARCHITECTURE.md.

use std::sync::Arc;

use chrono::{Days, Months, NaiveDate};
use erpcraft_kernel::document;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Decision, DynCommand, EntityMeta, Error, FieldMeta,
    FieldType, JsonSchema, Migration, Module, PolicyPoint, Result, Utc, async_trait, command,
    schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Asked before a run posts an invoice it made.
pub const POST: &str = "recurring_invoice.post";

pub struct RecurringModule;

impl Module for RecurringModule {
    fn name(&self) -> &'static str {
        "recurring"
    }

    fn label(&self) -> String {
        "Recurring invoices".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["sales", "tax"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "recurring",
            sql: include_str!("../migrations/0001_recurring.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("recurring_invoice", "Recurring invoice")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                        .required(),
                )
                .field(
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    )
                    .help("The accounting currency when empty."),
                )
                .field(FieldMeta::new(
                    "payment_terms_id",
                    "Payment terms",
                    FieldType::link("payment_terms"),
                ))
                .field(
                    FieldMeta::new(
                        "frequency",
                        "Frequency",
                        FieldType::labelled(&[
                            ("weekly", "Weekly"),
                            ("monthly", "Monthly"),
                            ("quarterly", "Quarterly"),
                            ("yearly", "Yearly"),
                        ]),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new("every", "Every", FieldType::Int).help(
                        "Every so many of the frequency: 2 with monthly is every two months.",
                    ),
                )
                .field(
                    FieldMeta::new("starts_on", "First invoice", FieldType::Date)
                        .required()
                        .help("The others are counted from it."),
                )
                .field(FieldMeta::new("next_date", "Next invoice", FieldType::Date).readonly())
                .field(FieldMeta::new("end_date", "Ends on", FieldType::Date))
                .field(
                    FieldMeta::new("max_count", "At most", FieldType::Int)
                        .help("Invoices to make in all; empty: until it ends."),
                )
                .field(FieldMeta::new("count", "Made", FieldType::Int).readonly())
                .field(FieldMeta::new("post", "Post", FieldType::Bool).help(
                    "Post the invoices it makes, as far as the policy \
                         recurring_invoice.post allows; otherwise leave them as drafts.",
                ))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .company_field("company_id"),
            EntityMeta::new("recurring_invoice_line", "Recurring invoice line")
                .field(
                    FieldMeta::new(
                        "recurring_invoice_id",
                        "Recurring invoice",
                        FieldType::link("recurring_invoice"),
                    )
                    .required()
                    .immutable(),
                )
                .field(FieldMeta::new("line_no", "Line", FieldType::Int))
                .field(FieldMeta::new(
                    "charge_id",
                    "Charge",
                    FieldType::link("charge"),
                ))
                .field(
                    FieldMeta::new("account_id", "Account", FieldType::link("account"))
                        .help("Or the revenue account, with a description."),
                )
                .field(FieldMeta::new(
                    "description",
                    "Description",
                    FieldType::Text,
                ))
                .field(FieldMeta::new("qty", "Quantity", FieldType::Decimal).quantity())
                .field(
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .required(),
                )
                .field(
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                        .help("Empty: what the tax rules propose."),
                )
                .field(created())
                .order_by(&["recurring_invoice_id", "line_no"])
                .child_of("recurring_invoice", "recurring_invoice_id"),
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                POST,
                "Asked by a recurring invoice run for each invoice of a template that posts what \
                 it makes: may it be posted without a person looking at it? Unless configured \
                 otherwise it may. Refused, the invoice is left as a draft; for example \
                 refuse when `facts.total > 5000`.",
            )
            .number("total", "The invoice's net total")
            .as_money(Some("currency"))
            .text("currency", "The invoice's currency")
            .text("customer", "The customer's code")
            .text("template", "The recurring invoice's code")
            .refusal("this invoice is left for a person to post"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Run)]
    }
}

/// The date of a template's invoice number `n` (from 0).
fn nth(starts_on: NaiveDate, frequency: &str, every: i32, n: i32) -> Option<NaiveDate> {
    let steps = u32::try_from(every.checked_mul(n)?).ok()?;
    match frequency {
        "weekly" => starts_on.checked_add_days(Days::new(u64::from(steps) * 7)),
        "monthly" => starts_on.checked_add_months(Months::new(steps)),
        "quarterly" => starts_on.checked_add_months(Months::new(steps.checked_mul(3)?)),
        "yearly" => starts_on.checked_add_months(Months::new(steps.checked_mul(12)?)),
        _ => None,
    }
}

#[derive(Debug, sqlx::FromRow)]
struct Template {
    id: Uuid,
    code: String,
    customer_id: Uuid,
    currency: Option<String>,
    payment_terms_id: Option<Uuid>,
    frequency: String,
    every: i32,
    starts_on: NaiveDate,
    next_date: NaiveDate,
    end_date: Option<NaiveDate>,
    max_count: Option<i32>,
    count: i32,
    post: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunInput {
    /// The company whose recurring invoices to run.
    company_id: Uuid,
    /// Make what is due up to this day; today when omitted.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema)]
struct Made {
    recurring_invoice_id: Uuid,
    code: String,
    invoice_id: Uuid,
    invoice_date: NaiveDate,
    /// Posted; else left as a draft.
    posted: bool,
}

#[derive(Serialize, JsonSchema)]
struct RunOutput {
    invoices: Vec<Made>,
}

struct Run;

#[async_trait]
impl Command for Run {
    type Input = RunInput;
    type Output = RunOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "recurring_invoice.run",
            "Make the invoices of a company's recurring invoices that are due up to `as_of` \
             (today by default), each date once, catching up on missed ones: an invoice without \
             an order with the template's lines, dated its date, drafted, and posted where the \
             template says so and the policy recurring_invoice.post allows. A template that \
             reaches its count or passes its end date is made inactive. All or nothing.",
        )
        .permission("recurring_invoice:run")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RunInput) -> Result<RunOutput> {
        ctx.require("recurring_invoice:run", input.company_id)?;
        ctx.require("sales_invoice:create", input.company_id)?;
        let as_of = input.as_of.unwrap_or_else(|| Utc::now().date_naive());
        let templates: Vec<Template> = sqlx::query_as(
            "SELECT id, code, customer_id, currency, payment_terms_id, frequency, every,
                    starts_on, next_date, end_date, max_count, count, post
             FROM recurring_invoice
             WHERE company_id = $1 AND active AND next_date <= $2
             ORDER BY code FOR UPDATE",
        )
        .bind(input.company_id)
        .bind(as_of)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let post = ctx
            .kernel
            .registry()
            .command("sales_invoice.post")?
            .handler
            .clone();
        let mut made = vec![];
        for template in templates {
            let lines: Vec<(
                Option<Uuid>,
                Option<Uuid>,
                Option<String>,
                Decimal,
                Decimal,
                Option<Uuid>,
            )> = sqlx::query_as(
                "SELECT charge_id, account_id, description, qty, unit_price, tax_code_id
                     FROM recurring_invoice_line WHERE recurring_invoice_id = $1
                     ORDER BY line_no, created_at",
            )
            .bind(template.id)
            .fetch_all(&mut *ctx.conn)
            .await?;
            if lines.is_empty() {
                return Err(Error::validation(format!(
                    "recurring invoice {} has no lines",
                    template.code
                )));
            }
            let customer: String = sqlx::query_scalar("SELECT code FROM customer WHERE id = $1")
                .bind(template.customer_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
            let rows: Vec<Value> = lines
                .iter()
                .map(|l| {
                    json!({ "charge_id": l.0, "account_id": l.1, "description": l.2,
                            "qty": l.3, "unit_price": l.4, "tax_code_id": l.5 })
                })
                .collect();
            let total: Decimal = lines.iter().map(|l| l.3 * l.4).sum();
            let mut count = template.count;
            let mut next = template.next_date;
            let ended = |count: i32, next: NaiveDate| {
                template.max_count.is_some_and(|m| count >= m)
                    || template.end_date.is_some_and(|e| next > e)
            };
            while next <= as_of && !ended(count, next) {
                let mut body = json!({ "customer_id": template.customer_id, "lines": rows });
                if let Some(currency) = &template.currency {
                    body["currency"] = json!(currency);
                }
                if let Some(terms) = template.payment_terms_id {
                    body["payment_terms_id"] = json!(terms);
                }
                let draft = document::create(
                    ctx,
                    document::NewDocument {
                        doc_type: "sales_invoice".into(),
                        company_id: input.company_id,
                        doc_date: next,
                        memo: Some(format!("Recurring invoice {}", template.code)),
                        body,
                    },
                )
                .await?;
                let invoice = draft.header.id;
                let mut posted = false;
                if template.post {
                    let currency = template.currency.clone().unwrap_or_default();
                    let ruling = ctx.policy(POST, input.company_id).await?.ruling(json!({
                        "total": total.round_dp(4).to_string(),
                        "currency": currency,
                        "customer": customer,
                        "template": template.code,
                    }))?;
                    if ruling.decision == Decision::Allowed {
                        post.run_json(ctx, json!({ "id": invoice })).await?;
                        posted = true;
                    }
                }
                sqlx::query(
                    "INSERT INTO recurring_invoice_run (recurring_invoice_id, invoice_date,
                         invoice_id, posted)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(template.id)
                .bind(next)
                .bind(invoice)
                .bind(posted)
                .execute(&mut *ctx.conn)
                .await?;
                made.push(Made {
                    recurring_invoice_id: template.id,
                    code: template.code.clone(),
                    invoice_id: invoice,
                    invoice_date: next,
                    posted,
                });
                count += 1;
                next = nth(
                    template.starts_on,
                    &template.frequency,
                    template.every,
                    count,
                )
                .ok_or_else(|| Error::validation("the schedule runs past the calendar"))?;
            }
            sqlx::query(
                "UPDATE recurring_invoice SET count = $2, next_date = $3, active = $4 WHERE id = $1",
            )
            .bind(template.id)
            .bind(count)
            .bind(next)
            .bind(!ended(count, next))
            .execute(&mut *ctx.conn)
            .await?;
            ctx.touch("recurring_invoice", template.id);
        }
        Ok(RunOutput { invoices: made })
    }
}
