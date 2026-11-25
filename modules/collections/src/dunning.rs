//! Dunning: levels, holds and payment plans, the run that proposes what
//! reached a new level, and the letters posting it makes.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use erpcraft_gl::GlLine;
use erpcraft_kernel::document::{self, Document, DocumentChange, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, Error, FieldMeta, FieldType,
    JsonSchema, PgConnection, Result, StateMeta, async_trait, company, new_id, round_money,
    schemars,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::WAIVE;

const RUN: &str = "dunning_run";
const LETTER: &str = "dunning_letter";
const POST: &str = "dunning_run.post";

/// A company's dunning level.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Level {
    level: i32,
    days_overdue: i32,
    fee: Decimal,
    fee_account_id: Option<Uuid>,
    fee_taxable: bool,
    interest_rate: Decimal,
    interest_account_id: Option<Uuid>,
    pay_within_days: i32,
    subject: Option<String>,
    text: Option<String>,
    print_template_id: Option<Uuid>,
}

async fn levels(conn: &mut PgConnection, company_id: Uuid) -> Result<Vec<Level>> {
    Ok(sqlx::query_as(
        "SELECT level, days_overdue, fee, fee_account_id, fee_taxable, interest_rate,
                interest_account_id, pay_within_days, subject, text, print_template_id
         FROM dunning_level WHERE company_id = $1 ORDER BY level",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?)
}

// ------------------------------------------------------------ the run

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunBody {
    #[serde(default)]
    lines: Vec<LineInput>,
    #[serde(default)]
    letters: Vec<LetterInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    id: Option<Uuid>,
    open_item_id: Uuid,
    /// None: what is proposed.
    interest: Option<Decimal>,
    #[serde(default)]
    skip: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LetterInput {
    id: Option<Uuid>,
    customer_id: Uuid,
    /// None: what is proposed.
    fee: Option<Decimal>,
    #[serde(default)]
    skip: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    open_item_id: Uuid,
    customer_id: Uuid,
    due_date: Option<NaiveDate>,
    days_overdue: i32,
    currency: String,
    open: Decimal,
    carried: Decimal,
    level: i32,
    interest_from: Option<NaiveDate>,
    proposed_interest: Decimal,
    interest: Decimal,
    skip: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct RunLetter {
    id: Uuid,
    line_no: i32,
    customer_id: Uuid,
    level: i32,
    proposed_fee: Decimal,
    fee: Decimal,
    skip: bool,
    letter_id: Option<Uuid>,
}

async fn run_lines(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, open_item_id, customer_id, due_date, days_overdue, currency, open,
                carried, level, interest_from, proposed_interest, interest, skip
         FROM dunning_run_line WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

async fn run_letters(conn: &mut PgConnection, id: Uuid) -> Result<Vec<RunLetter>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, customer_id, level, proposed_fee, fee, skip, letter_id
         FROM dunning_run_letter WHERE document_id = $1 ORDER BY line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

fn money(name: &str, label: &str) -> FieldMeta {
    FieldMeta::new(name, label, FieldType::Decimal).money()
}

pub(crate) struct Run;

#[async_trait]
impl DocumentHandler for Run {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: RUN.into(),
            module: String::new(),
            label: "Dunning run".into(),
            plural: "Dunning runs".into(),
            number_prefix: "DUN".into(),
            fields: vec![],
            collections: vec![
                CollectionMeta {
                    name: "lines".into(),
                    label: "Items".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new(
                            "open_item_id",
                            "Open item",
                            FieldType::Lookup(
                                erpcraft_openitems::lookup()
                                    .field("company_id", "company_id")
                                    .value("kind", "receivable"),
                            ),
                        )
                        .required(),
                        FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                            .readonly(),
                        FieldMeta::new("due_date", "Due", FieldType::Date).readonly(),
                        FieldMeta::new("days_overdue", "Days overdue", FieldType::Int).readonly(),
                        FieldMeta::new(
                            "currency",
                            "Currency",
                            FieldType::link_by("currency", "code"),
                        )
                        .readonly(),
                        FieldMeta::new("open", "Open", FieldType::Decimal)
                            .money()
                            .currency_in("currency")
                            .readonly(),
                        money("carried", "Carried at").readonly().help(
                            "What is open in the accounting currency: what interest is charged on.",
                        ),
                        FieldMeta::new("level", "Level", FieldType::Int)
                            .readonly()
                            .help("The level the item reaches."),
                        FieldMeta::new("interest_from", "Interest from", FieldType::Date)
                            .readonly(),
                        money("proposed_interest", "Interest proposed").readonly(),
                        money("interest", "Interest").help(
                            "In the accounting currency. Less than proposed is waiving, which \
                             the policy dunning.waive decides.",
                        ),
                        FieldMeta::new("skip", "Skip", FieldType::Bool)
                            .help("Keep the item on the run but do not dun it."),
                    ],
                },
                CollectionMeta {
                    name: "letters".into(),
                    label: "Letters".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                            .required(),
                        FieldMeta::new("level", "Level", FieldType::Int).readonly(),
                        money("proposed_fee", "Fee proposed").readonly(),
                        money("fee", "Fee").help(
                            "In the accounting currency. Less than proposed is waiving, which \
                             the policy dunning.waive decides.",
                        ),
                        FieldMeta::new("skip", "Skip", FieldType::Bool)
                            .help("Send this customer no letter now."),
                        FieldMeta::new("letter_id", "Letter", FieldType::document(LETTER))
                            .readonly(),
                    ],
                },
            ],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("posted", "Posted"),
                StateMeta::locked("cancelled", "Cancelled"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "posted".into()),
                ("draft".into(), "cancelled".into()),
            ],
            summary: vec![
                FieldMeta::new("letters", "Letters", FieldType::Int).readonly(),
                FieldMeta::new("items", "Items", FieldType::Int).readonly(),
                money("fees", "Fees").readonly(),
                money("interest", "Interest").readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: RunBody = erpcraft_kernel::input::parse(body.clone(), "dunning run body")?;
        let conn = &mut *ctx.conn;
        let levels = levels(&mut *conn, doc.company_id).await?;
        if levels.is_empty() && !body.lines.is_empty() {
            return Err(Error::validation(
                "the company has no dunning levels: set them up first",
            ));
        }
        let base = company::base_currency(&mut *conn, doc.company_id).await?;
        let decimals = company::currency_decimals(&mut *conn, doc.company_id).await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.open_item_id).collect();
        let items = erpcraft_openitems::lock(&mut *conn, doc.company_id, &ids).await?;
        // The level each item last had, and the day interest was last
        // charged to.
        let history: Vec<(Uuid, i32, Option<NaiveDate>)> = sqlx::query_as(
            "SELECT open_item_id, max(level), max(interest_to) FROM dunning_history
             WHERE open_item_id = ANY ($1) GROUP BY open_item_id",
        )
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
        let mut rows = vec![];
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            let fail = |field: &str, msg: String| Error::line("lines", n, Some(field), msg);
            let Some(item) = items.iter().find(|i| i.id == line.open_item_id) else {
                return Err(fail(
                    "open_item_id",
                    "the item must be an open item of the company".into(),
                ));
            };
            if item.kind != "receivable" || item.party_type != "customer" {
                return Err(fail(
                    "open_item_id",
                    "a dunning run duns what customers owe".into(),
                ));
            }
            if item.txn_open <= Decimal::ZERO {
                return Err(fail(
                    "open_item_id",
                    "the item owes nothing: it is settled or a credit".into(),
                ));
            }
            let due = item.due_date.unwrap_or(item.doc_date);
            let days = (doc.doc_date - due).num_days();
            let (last, interest_to) = history
                .iter()
                .find(|h| h.0 == item.id)
                .map(|h| (h.1, h.2))
                .unwrap_or((0, None));
            let Some(level) = levels
                .iter()
                .rev()
                .find(|l| i64::from(l.days_overdue) <= days && l.days_overdue >= 0 && days > 0)
            else {
                return Err(fail(
                    "open_item_id",
                    format!(
                        "the item is {} days overdue: it reaches no dunning level",
                        days.max(0)
                    ),
                ));
            };
            if level.level <= last {
                return Err(fail(
                    "open_item_id",
                    format!("the item has had level {last} and reaches no higher one yet"),
                ));
            }
            let from = interest_to
                .map(|d| d + Days::new(1))
                .unwrap_or(due + Days::new(1))
                .max(due + Days::new(1));
            let interest_days = (doc.doc_date - from).num_days() + 1;
            let proposed = if level.interest_rate > Decimal::ZERO && interest_days > 0 {
                round_money(
                    item.open * level.interest_rate / Decimal::ONE_HUNDRED
                        * Decimal::from(interest_days)
                        / Decimal::from(365),
                    decimals,
                )
            } else {
                Decimal::ZERO
            };
            let interest = line.interest.unwrap_or(proposed);
            if interest.is_sign_negative() || interest.normalize().scale() > decimals {
                return Err(fail(
                    "interest",
                    format!("interest is not below zero, with at most {decimals} decimal places"),
                ));
            }
            rows.push((
                line,
                item.party_id,
                item.due_date,
                days,
                item.currency.clone(),
                item.txn_open,
                item.open,
                level.level,
                (interest_days > 0).then_some(from),
                proposed,
                interest,
            ));
        }
        let _ = base;
        let line_ids = document::line_ids(
            &mut *conn,
            "dunning_run_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM dunning_run_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *conn)
            .await?;
        for (n, (row, id)) in rows.iter().zip(&line_ids).enumerate() {
            sqlx::query(
                "INSERT INTO dunning_run_line (id, document_id, line_no, open_item_id, customer_id,
                     due_date, days_overdue, currency, open, carried, level, interest_from,
                     proposed_interest, interest, skip)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
            )
            .bind(id)
            .bind(doc.id)
            .bind(n as i32 + 1)
            .bind(row.0.open_item_id)
            .bind(row.1)
            .bind(row.2)
            .bind(row.3 as i32)
            .bind(&row.4)
            .bind(row.5)
            .bind(row.6)
            .bind(row.7)
            .bind(row.8)
            .bind(row.9)
            .bind(row.10)
            .bind(row.0.skip)
            .execute(&mut *conn)
            .await
            .map_err(|e| match Error::from(e) {
                Error::Conflict(_) => Error::line(
                    "lines",
                    n + 1,
                    Some("open_item_id"),
                    "the item is on the run twice",
                ),
                other => other,
            })?;
        }
        // A letter per customer of the lines, at the highest level of its
        // lines, with that level's fee unless the body gives one.
        let mut customers: Vec<(Uuid, i32)> = vec![];
        for row in &rows {
            match customers.iter_mut().find(|c| c.0 == row.1) {
                Some(c) => c.1 = c.1.max(row.7),
                None => customers.push((row.1, row.7)),
            }
        }
        for (index, letter) in body.letters.iter().enumerate() {
            if !customers.iter().any(|c| c.0 == letter.customer_id) {
                return Err(Error::line(
                    "letters",
                    index + 1,
                    Some("customer_id"),
                    "the customer has no item on the run",
                ));
            }
            if letter
                .fee
                .is_some_and(|f| f.is_sign_negative() || f.normalize().scale() > decimals)
            {
                return Err(Error::line(
                    "letters",
                    index + 1,
                    Some("fee"),
                    format!("a fee is not below zero, with at most {decimals} decimal places"),
                ));
            }
        }
        let given: Vec<Option<Uuid>> = customers
            .iter()
            .map(|c| {
                body.letters
                    .iter()
                    .find(|l| l.customer_id == c.0)
                    .and_then(|l| l.id)
            })
            .collect();
        let letter_ids =
            document::line_ids(&mut *conn, "dunning_run_letter", doc.id, given).await?;
        sqlx::query("DELETE FROM dunning_run_letter WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *conn)
            .await?;
        for (n, ((customer, level), id)) in customers.iter().zip(&letter_ids).enumerate() {
            let proposed = levels
                .iter()
                .find(|l| l.level == *level)
                .map(|l| l.fee)
                .unwrap_or_default();
            let given = body.letters.iter().find(|l| l.customer_id == *customer);
            sqlx::query(
                "INSERT INTO dunning_run_letter (id, document_id, line_no, customer_id, level,
                     proposed_fee, fee, skip)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(doc.id)
            .bind(n as i32 + 1)
            .bind(customer)
            .bind(level)
            .bind(proposed)
            .bind(given.and_then(|l| l.fee).unwrap_or(proposed))
            .bind(given.is_some_and(|l| l.skip))
            .execute(&mut *conn)
            .await?;
        }
        Ok(())
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let letters: Vec<&Value> = document::body::lines(body, "letters")
            .iter()
            .filter(|l| l["skip"] != json!(true))
            .collect();
        let sent: Vec<String> = letters
            .iter()
            .map(|l| l["customer_id"].to_string())
            .collect();
        let lines: Vec<&Value> = document::body::lines(body, "lines")
            .iter()
            .filter(|l| l["skip"] != json!(true) && sent.contains(&l["customer_id"].to_string()))
            .collect();
        Ok(json!({
            "letters": letters.len(),
            "items": lines.len(),
            "fees": letters.iter().map(|l| document::body::amount(&l["fee"])).sum::<Decimal>(),
            "interest": lines.iter().map(|l| document::body::amount(&l["interest"])).sum::<Decimal>(),
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        Ok(json!({
            "lines": run_lines(conn, doc.id).await?,
            "letters": run_letters(conn, doc.id).await?,
        }))
    }
}

// ------------------------------------------------------------ the letter

#[derive(Debug, Serialize, sqlx::FromRow)]
struct LetterHeader {
    run_id: Option<Uuid>,
    customer_id: Uuid,
    level: i32,
    fee: Decimal,
    tax: Decimal,
    interest: Decimal,
    due_date: Option<NaiveDate>,
    subject: Option<String>,
    text: Option<String>,
    print_template_id: Option<Uuid>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct LetterLine {
    id: Uuid,
    line_no: i32,
    open_item_id: Uuid,
    number: Option<String>,
    doc_date: NaiveDate,
    due_date: Option<NaiveDate>,
    days_overdue: i32,
    currency: String,
    open: Decimal,
    level: i32,
    interest: Decimal,
    interest_from: Option<NaiveDate>,
}

pub(crate) struct Letter;

#[async_trait]
impl DocumentHandler for Letter {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: LETTER.into(),
            module: String::new(),
            label: "Dunning letter".into(),
            plural: "Dunning letters".into(),
            number_prefix: "DL".into(),
            fields: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).required(),
                FieldMeta::new("run_id", "Dunning run", FieldType::document(RUN)).readonly(),
                FieldMeta::new("level", "Level", FieldType::Int).readonly(),
                FieldMeta::new("subject", "Subject", FieldType::Text).readonly(),
                FieldMeta::new("text", "Text", FieldType::Text).readonly(),
                money("fee", "Fee").readonly(),
                money("tax", "Tax on the fee").readonly(),
                money("interest", "Interest").readonly(),
                FieldMeta::new("due_date", "Pay by", FieldType::Date).readonly(),
                FieldMeta::new(
                    "print_template_id",
                    "Template",
                    FieldType::link("print_template"),
                )
                .readonly(),
            ],
            collections: vec![CollectionMeta {
                name: "lines".into(),
                label: "Overdue items".into(),
                fields: vec![
                    FieldMeta::line_id(),
                    FieldMeta::line_no(),
                    FieldMeta::new(
                        "open_item_id",
                        "Open item",
                        FieldType::Lookup(
                            erpcraft_openitems::lookup().field("company_id", "company_id"),
                        ),
                    )
                    .readonly(),
                    FieldMeta::new("number", "Invoice", FieldType::Text).readonly(),
                    FieldMeta::new("doc_date", "Date", FieldType::Date).readonly(),
                    FieldMeta::new("due_date", "Due", FieldType::Date).readonly(),
                    FieldMeta::new("days_overdue", "Days overdue", FieldType::Int).readonly(),
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    )
                    .readonly(),
                    FieldMeta::new("open", "Open", FieldType::Decimal)
                        .money()
                        .currency_in("currency")
                        .readonly(),
                    FieldMeta::new("level", "Level", FieldType::Int).readonly(),
                    money("interest", "Interest").readonly(),
                    FieldMeta::new("interest_from", "Interest from", FieldType::Date).readonly(),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("posted", "Posted"),
            ],
            initial_state: "draft".into(),
            transitions: vec![("draft".into(), "posted".into())],
            summary: vec![
                FieldMeta::new("customer_id", "Customer", FieldType::link("customer")).readonly(),
                FieldMeta::new("level", "Level", FieldType::Int).readonly(),
                money("total", "Charged").readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        // A letter is what posting a run makes, and only that.
        if ctx.command != POST {
            return Err(Error::validation(
                "a dunning letter is made by posting a dunning run",
            ));
        }
        let customer = document::body::uuid(&body["customer_id"])
            .ok_or_else(|| Error::validation("a letter names its customer"))?;
        sqlx::query(
            "INSERT INTO dunning_letter (document_id, customer_id, level) VALUES ($1, $2, 1)
             ON CONFLICT (document_id) DO NOTHING",
        )
        .bind(doc.id)
        .bind(customer)
        .execute(&mut *ctx.conn)
        .await?;
        Ok(())
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let total = document::body::amount(&body["fee"])
            + document::body::amount(&body["tax"])
            + document::body::amount(&body["interest"]);
        Ok(json!({
            "customer_id": body["customer_id"],
            "level": body["level"],
            "total": total,
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header: Option<LetterHeader> = sqlx::query_as(
            "SELECT run_id, customer_id, level, fee, tax, interest, due_date, subject, text,
                    print_template_id
             FROM dunning_letter WHERE document_id = $1",
        )
        .bind(doc.id)
        .fetch_optional(&mut *conn)
        .await?;
        let lines: Vec<LetterLine> = sqlx::query_as(
            "SELECT id, line_no, open_item_id, number, doc_date, due_date, days_overdue, currency,
                    open, level, interest, interest_from
             FROM dunning_letter_line WHERE document_id = $1 ORDER BY line_no",
        )
        .bind(doc.id)
        .fetch_all(&mut *conn)
        .await?;
        let mut body = serde_json::to_value(&header).map_err(Error::internal)?;
        if body.is_null() {
            body = json!({});
        }
        body["lines"] = serde_json::to_value(&lines).map_err(Error::internal)?;
        Ok(body)
    }
}

// ------------------------------------------------------------ commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdInput {
    /// Dunning run id.
    id: Uuid,
}

/// Why a customer or item may not be dunned on a day: held, or on a
/// payment plan. `None` when it may.
async fn stopped(
    conn: &mut PgConnection,
    company_id: Uuid,
    customer: Uuid,
    item: Option<Uuid>,
    day: NaiveDate,
) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT why FROM (
             SELECT 'on hold: ' || reason AS why, 1 AS o FROM dunning_hold
             WHERE company_id = $1 AND active
               AND (customer_id = $2 OR ($3::uuid IS NOT NULL AND open_item_id = $3))
             UNION ALL
             SELECT 'on a payment plan', 2 FROM payment_plan
             WHERE company_id = $1 AND customer_id = $2 AND active AND starts_on <= $4
               AND (ends_on IS NULL OR ends_on >= $4)
         ) s ORDER BY o LIMIT 1",
    )
    .bind(company_id)
    .bind(customer)
    .bind(item)
    .bind(day)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Fills a draft run with what reached a new level.
pub(crate) struct Propose;

#[async_trait]
impl Command for Propose {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "dunning_run.propose",
            "Fill a draft dunning run with every overdue customer item that reached a dunning \
             level above the one it last had, as of the run's date, with the interest its level \
             charges, and a letter per customer with its level's fee. Held customers and items, \
             customers on a payment plan, customers who owe nothing once their credits count, \
             and items on another run that is not posted or cancelled are left out. Replaces the \
             run's lines; review them, then post.",
        )
        .permission("dunning_run:update")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let doc = document::lock(ctx, RUN, input.id, "update").await?;
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} dunning run cannot be proposed again",
                doc.status
            )));
        }
        ctx.require("open_item:read", doc.company_id)?;
        let levels = levels(&mut *ctx.conn, doc.company_id).await?;
        let Some(first) = levels.first() else {
            return Err(Error::validation(
                "the company has no dunning levels: set them up first",
            ));
        };
        let candidates: Vec<(Uuid, Uuid, i64, i32)> = sqlx::query_as(
            "SELECT i.id, i.party_id, ($2::date - COALESCE(i.due_date, i.doc_date))::bigint,
                    COALESCE((SELECT max(h.level) FROM dunning_history h
                              WHERE h.open_item_id = i.id), 0)
             FROM open_item i
             WHERE i.company_id = $1 AND i.kind = 'receivable' AND i.party_type = 'customer'
               AND COALESCE(i.txn_amount, i.amount) <> COALESCE(i.txn_settled, i.settled)
               AND COALESCE(i.due_date, i.doc_date) < $2
               AND NOT EXISTS (SELECT 1 FROM dunning_run_line l JOIN document d ON d.id = l.document_id
                               WHERE l.open_item_id = i.id AND d.id <> $3 AND d.status = 'draft')
             ORDER BY i.party_id, COALESCE(i.due_date, i.doc_date), i.id",
        )
        .bind(doc.company_id)
        .bind(doc.doc_date)
        .bind(doc.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // What each customer owes all told: credits count against it.
        let owed: Vec<(Uuid, Decimal)> = sqlx::query_as(
            "SELECT party_id, sum(amount - settled) FROM open_item
             WHERE company_id = $1 AND kind = 'receivable' AND party_type = 'customer'
             GROUP BY party_id",
        )
        .bind(doc.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut lines = vec![];
        for (item, customer, days, last) in candidates {
            if days < i64::from(first.days_overdue) || days <= 0 {
                continue;
            }
            let reached = levels
                .iter()
                .rev()
                .find(|l| i64::from(l.days_overdue) <= days)
                .map_or(0, |l| l.level);
            if reached <= last {
                continue;
            }
            if !owed
                .iter()
                .any(|(c, o)| *c == customer && *o > Decimal::ZERO)
            {
                continue;
            }
            if stopped(
                &mut *ctx.conn,
                doc.company_id,
                customer,
                Some(item),
                doc.doc_date,
            )
            .await?
            .is_some()
            {
                continue;
            }
            lines.push(json!({ "open_item_id": item }));
        }
        document::update(
            ctx,
            DocumentChange {
                doc_type: RUN.into(),
                id: doc.id,
                version: doc.version,
                doc_date: None,
                memo: None,
                body: Some(json!({ "lines": lines, "letters": [] })),
            },
        )
        .await
    }
}

pub(crate) struct Cancel;

#[async_trait]
impl Command for Cancel {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "dunning_run.cancel",
            "Cancel a draft dunning run: its items can be proposed again.",
        )
        .permission("dunning_run:cancel")
        .from_states(&["draft"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RUN, input.id, "cancel").await?;
        if doc.status == "cancelled" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} dunning run cannot be cancelled",
                doc.status
            )));
        }
        document::transition(ctx, &mut doc, "cancelled").await?;
        document::view(ctx, doc).await
    }
}

/// Posts a reviewed run: a letter per customer.
pub(crate) struct Post;

#[async_trait]
impl Command for Post {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            POST,
            "Post a reviewed dunning run: for each letter not skipped, a dunning letter of the \
             customer's items not skipped, posted on the run's date: its fee (taxed where the \
             level says so) and interest to their accounts, owed by the customer as one open item \
             due within the level's days; each item's new level is kept in its dunning history. \
             A fee or interest below what was proposed asks the policy dunning.waive. Holds and \
             payment plans are checked again. All or nothing.",
        )
        .permission("dunning_run:post")
        .from_states(&["draft"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RUN, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} dunning run cannot be posted",
                doc.status
            )));
        }
        let levels = levels(&mut *ctx.conn, doc.company_id).await?;
        let lines = run_lines(&mut *ctx.conn, doc.id).await?;
        let letters = run_letters(&mut *ctx.conn, doc.id).await?;
        let mut by_customer: BTreeMap<Uuid, Vec<&Line>> = BTreeMap::new();
        for line in lines.iter().filter(|l| !l.skip) {
            by_customer.entry(line.customer_id).or_default().push(line);
        }
        let to_send: Vec<&RunLetter> = letters
            .iter()
            .filter(|l| !l.skip && by_customer.contains_key(&l.customer_id))
            .collect();
        if to_send.is_empty() {
            return Err(Error::validation(
                "the run duns nobody: propose it, or take lines off skip",
            ));
        }
        let receivable: Uuid = sqlx::query_scalar(
            "SELECT receivable_account_id FROM sales_settings WHERE company_id = $1",
        )
        .bind(doc.company_id)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| {
            Error::validation("the company has no sales settings: they name the receivable account")
        })?;
        for run_letter in to_send {
            let items = &by_customer[&run_letter.customer_id];
            let code: String = sqlx::query_scalar("SELECT code FROM customer WHERE id = $1")
                .bind(run_letter.customer_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
            // Held, or on a plan, since the run was proposed.
            for item in items {
                if let Some(why) = stopped(
                    &mut *ctx.conn,
                    doc.company_id,
                    item.customer_id,
                    Some(item.open_item_id),
                    doc.doc_date,
                )
                .await?
                {
                    return Err(Error::validation(format!(
                        "customer {code} is {why}: lift it, or skip its letter"
                    )));
                }
            }
            let level = run_letter.level;
            let level_of = levels
                .iter()
                .find(|l| l.level == level)
                .ok_or_else(|| Error::validation(format!("dunning level {level} is gone")))?;
            let interest: Decimal = items.iter().map(|l| l.interest).sum();
            let proposed_interest: Decimal = items.iter().map(|l| l.proposed_interest).sum();
            let fee_waived = (run_letter.proposed_fee - run_letter.fee).max(Decimal::ZERO);
            let interest_waived = (proposed_interest - interest).max(Decimal::ZERO);
            if !fee_waived.is_zero() || !interest_waived.is_zero() {
                ctx.decide(
                    WAIVE,
                    doc.company_id,
                    json!({
                        "fee_waived": fee_waived.to_string(),
                        "interest_waived": interest_waived.to_string(),
                        "level": level,
                        "customer": code,
                    }),
                )
                .await?;
            }
            let letter =
                write_letter(ctx, &doc, run_letter, level_of, items, interest, receivable).await?;
            document::link(ctx, doc.id, letter, "dunning_letter").await?;
            sqlx::query("UPDATE dunning_run_letter SET letter_id = $2 WHERE id = $1")
                .bind(run_letter.id)
                .bind(letter)
                .execute(&mut *ctx.conn)
                .await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

/// Makes, posts and records one customer's letter. Returns its id.
async fn write_letter(
    ctx: &mut Ctx<'_>,
    run: &Document,
    run_letter: &RunLetter,
    level: &Level,
    items: &[&Line],
    interest: Decimal,
    receivable: Uuid,
) -> Result<Uuid> {
    let customer = run_letter.customer_id;
    let fee = run_letter.fee;
    for (amount, account, what) in [
        (fee, level.fee_account_id, "fee"),
        (interest, level.interest_account_id, "interest"),
    ] {
        if amount.is_zero() {
            continue;
        }
        let account = account.ok_or_else(|| {
            Error::validation(format!(
                "dunning level {} charges {what} but names no account for it",
                level.level
            ))
        })?;
        if let Some(why) =
            erpcraft_gl::line_account_refusal(&mut *ctx.conn, run.company_id, account).await?
        {
            return Err(Error::validation(format!(
                "the {what} account of dunning level {}: {why}",
                level.level
            )));
        }
    }
    let draft = document::create(
        ctx,
        document::NewDocument {
            doc_type: LETTER.into(),
            company_id: run.company_id,
            doc_date: run.doc_date,
            memo: run.number.as_ref().map(|n| format!("Dunning run {n}")),
            body: json!({ "customer_id": customer }),
        },
    )
    .await?;
    let letter_id = draft.header.id;
    let mut letter = document::lock(ctx, LETTER, letter_id, "post").await?;
    let due = run.doc_date + Days::new(level.pay_within_days.max(0) as u64);
    // The parties first: the tax on the fee reads where the customer is.
    erpcraft_party::snapshot_sale(
        &mut *ctx.conn,
        letter_id,
        run.company_id,
        customer,
        None,
        None,
    )
    .await?;
    let mut taxes = vec![];
    if !fee.is_zero() && level.fee_taxable {
        let place = erpcraft_party::ship_to(&mut *ctx.conn, "customer", customer, None, None)
            .await?
            .map(|a| erpcraft_tax::Place {
                country_code: a.country_code,
                region: a.region,
                postcode: a.postcode,
                city: a.city,
            });
        let consumer =
            !erpcraft_party::has_tax_id(&mut *ctx.conn, "customer", customer, "eu_vat").await?;
        let code = erpcraft_tax::propose(
            &mut *ctx.conn,
            &erpcraft_tax::Proposal {
                company_id: run.company_id,
                date: run.doc_date,
                side: erpcraft_tax::Side::Sale {
                    customer_id: customer,
                    consumer,
                },
                item_id: None,
                charge_id: None,
                place: place.as_ref(),
            },
        )
        .await?;
        if let Some(code) = code {
            taxes = erpcraft_tax::quote(
                ctx,
                run.company_id,
                code,
                erpcraft_tax::Direction::Collected,
                fee,
                None,
                None,
            )
            .await?;
        }
    }
    let tax = erpcraft_tax::Tax::total(&taxes);
    let total = fee + tax + interest;
    let line = |account: Uuid, debit: Decimal, credit: Decimal| GlLine {
        account_id: account,
        debit,
        credit,
        scopes: vec![("customer", customer)],
        ..GlLine::default()
    };
    if !total.is_zero() {
        let posting = document::create_posting(
            ctx,
            &letter,
            run.doc_date,
            Some(format!("Dunning level {}", level.level)),
        )
        .await?;
        let mut gl = vec![line(receivable, total, Decimal::ZERO)];
        if let (false, Some(account)) = (fee.is_zero(), level.fee_account_id) {
            gl.push(line(account, Decimal::ZERO, fee));
        }
        for t in &taxes {
            let (debit, credit) = t.debit_credit();
            if !(debit.is_zero() && credit.is_zero()) {
                gl.push(line(t.account_id, debit, credit));
            }
        }
        if let (false, Some(account)) = (interest.is_zero(), level.interest_account_id) {
            gl.push(line(account, Decimal::ZERO, interest));
        }
        erpcraft_gl::post(ctx, &posting, &gl).await?;
        erpcraft_tax::record(&mut *ctx.conn, &posting, &taxes).await?;
        erpcraft_openitems::open(
            &mut *ctx.conn,
            &posting,
            &NewOpenItem {
                kind: Kind::Receivable,
                party_type: "customer",
                party_id: customer,
                account_id: receivable,
                amount: total,
                due_date: Some(due),
                terms: None,
                foreign: None,
            },
        )
        .await?;
    }
    sqlx::query(
        "UPDATE dunning_letter SET run_id = $2, level = $3, fee = $4, tax = $5, interest = $6,
             due_date = $7, subject = $8, text = $9, print_template_id = $10
         WHERE document_id = $1",
    )
    .bind(letter_id)
    .bind(run.id)
    .bind(level.level)
    .bind(fee)
    .bind(tax)
    .bind(interest)
    .bind(due)
    .bind(&level.subject)
    .bind(&level.text)
    .bind(level.print_template_id)
    .execute(&mut *ctx.conn)
    .await?;
    for (n, item) in items.iter().enumerate() {
        let (number, doc_date): (Option<String>, NaiveDate) = sqlx::query_as(
            "SELECT d.number, i.doc_date FROM open_item i JOIN posting p ON p.id = i.posting_id
             LEFT JOIN document d ON d.id = p.document_id WHERE i.id = $1",
        )
        .bind(item.open_item_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        sqlx::query(
            "INSERT INTO dunning_letter_line (id, document_id, line_no, open_item_id, number,
                 doc_date, due_date, days_overdue, currency, open, level, interest, interest_from)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(new_id())
        .bind(letter_id)
        .bind(n as i32 + 1)
        .bind(item.open_item_id)
        .bind(&number)
        .bind(doc_date)
        .bind(item.due_date)
        .bind(item.days_overdue)
        .bind(&item.currency)
        .bind(item.open)
        .bind(item.level)
        .bind(item.interest)
        .bind(item.interest_from)
        .execute(&mut *ctx.conn)
        .await?;
        sqlx::query(
            "INSERT INTO dunning_history (id, company_id, open_item_id, customer_id, letter_id,
                 run_id, level, dunned_on, interest, interest_from, interest_to)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(new_id())
        .bind(run.company_id)
        .bind(item.open_item_id)
        .bind(customer)
        .bind(letter_id)
        .bind(run.id)
        .bind(item.level)
        .bind(run.doc_date)
        .bind(item.interest)
        .bind(item.interest_from.filter(|_| !item.interest.is_zero()))
        .bind((!item.interest.is_zero()).then_some(run.doc_date))
        .execute(&mut *ctx.conn)
        .await
        .map_err(|e| match Error::from(e) {
            Error::Conflict(_) => Error::validation(format!(
                "an item of customer {customer} was dunned at level {} meanwhile",
                item.level
            )),
            other => other,
        })?;
    }
    document::transition(ctx, &mut letter, "posted").await?;
    Ok(letter_id)
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Sent {
    letter_id: Uuid,
    /// The message recorded on the letter, when it was sent.
    message_id: Option<Uuid>,
    /// Why it was not sent, such as a customer without an email address.
    skipped: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SentOutput {
    id: Uuid,
    sent: Vec<Sent>,
}

/// Emails each letter of a posted run.
pub(crate) struct Send;

#[async_trait]
impl Command for Send {
    type Input = IdInput;
    type Output = SentOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "dunning_run.send",
            "Send the letters of a posted dunning run: each by email to its customer with \
             document.send, printed with its level's template, with its level's subject and text \
             as the message, to the customer's email on record. A customer without one is \
             listed as skipped. Takes the right to send dunning letters.",
        )
        .permission("dunning_run:read")
        .from_states(&["posted"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<SentOutput> {
        let doc = document::lock(ctx, RUN, input.id, "read").await?;
        if doc.status != "posted" {
            return Err(Error::state("letters are sent for a posted dunning run"));
        }
        let send = ctx
            .kernel
            .registry()
            .command("document.send")?
            .handler
            .clone();
        let parties = ctx
            .kernel
            .registry()
            .command("document.parties")?
            .handler
            .clone();
        let letters: Vec<(Uuid, Option<String>, Option<String>, Option<Uuid>)> = sqlx::query_as(
            "SELECT l.document_id, l.subject, l.text, l.print_template_id
             FROM dunning_run_letter r JOIN dunning_letter l ON l.document_id = r.letter_id
             WHERE r.document_id = $1 ORDER BY r.line_no",
        )
        .bind(doc.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut sent = vec![];
        for (letter, subject, text, template) in letters {
            let party = parties
                .run_json(ctx, json!({ "doc_type": LETTER, "id": letter }))
                .await?;
            let buyer = &party["buyer"];
            let address = ["contact_email", "electronic_address"]
                .iter()
                .find_map(|f| buyer[f].as_str().filter(|a| a.contains('@')));
            if address.is_none() {
                sent.push(Sent {
                    letter_id: letter,
                    message_id: None,
                    skipped: Some("the customer has no email address on record".into()),
                });
                continue;
            }
            let mut input = json!({ "doc_type": LETTER, "id": letter });
            if let Some(subject) = subject {
                input["subject"] = json!(subject);
            }
            if let Some(text) = text {
                input["message"] = json!(text);
            }
            if let Some(template) = template {
                input["template_id"] = json!(template);
            }
            let message = send.run_json(ctx, input).await?;
            sent.push(Sent {
                letter_id: letter,
                message_id: document::body::uuid(&message["id"]),
                skipped: None,
            });
        }
        Ok(SentOutput { id: doc.id, sent })
    }
}
