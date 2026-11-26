//! The commission run and the statements it makes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use erpcraft_gl::{Dimensions, GlLine};
use erpcraft_kernel::document::{self, Document, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DocumentHandler, Error,
    FieldMeta, FieldType, JsonSchema, PgConnection, PrimaryWhenSpec, Result, StateMeta,
    async_trait, company, new_id, schemars,
};
use erpcraft_openitems::{Kind, NewOpenItem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::POST;
use crate::plan::{Plan, earned, plan_for, plans};

pub(crate) const RUN: &str = "commission_run";
pub(crate) const STATEMENT: &str = "commission_statement";

fn money(name: &str, label: &str) -> FieldMeta {
    FieldMeta::new(name, label, FieldType::Decimal).money()
}

fn item_fields() -> Vec<FieldMeta> {
    vec![
        FieldMeta::line_id(),
        FieldMeta::line_no(),
        FieldMeta::new("sales_rep_id", "Salesperson", FieldType::link("sales_rep")).readonly(),
        FieldMeta::new("plan_id", "Plan", FieldType::link("commission_plan")).readonly(),
        FieldMeta::new("source_type", "Document type", FieldType::Text).readonly(),
        FieldMeta::new("source_number", "Document", FieldType::Text).readonly(),
        FieldMeta::new("earned_on", "Earned on", FieldType::Date)
            .readonly()
            .help("The date of the sale, credit or payment."),
        money("basis", "Basis").readonly(),
        money("commission", "Commission").readonly(),
    ]
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Line {
    id: Uuid,
    line_no: i32,
    sales_rep_id: Uuid,
    plan_id: Uuid,
    basis: Decimal,
    proposed: Decimal,
    commission: Decimal,
    #[sqlx(default)]
    adjusted: bool,
    statement_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
struct Item {
    id: Uuid,
    line_no: i32,
    sales_rep_id: Uuid,
    plan_id: Uuid,
    source_type: String,
    source_number: Option<String>,
    earned_on: NaiveDate,
    basis: Decimal,
    commission: Decimal,
    #[serde(skip)]
    dimension_set_id: Option<Uuid>,
}

async fn run_lines(conn: &mut PgConnection, id: Uuid, rep: Option<Uuid>) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT id, line_no, sales_rep_id, plan_id, basis, proposed, commission,
                commission <> proposed AS adjusted, statement_id
         FROM commission_run_line
         WHERE document_id = $1 AND ($2::uuid IS NULL OR sales_rep_id = $2) ORDER BY line_no",
    )
    .bind(id)
    .bind(rep)
    .fetch_all(&mut *conn)
    .await?)
}

async fn run_items(conn: &mut PgConnection, id: Uuid, rep: Option<Uuid>) -> Result<Vec<Item>> {
    Ok(sqlx::query_as(
        "SELECT i.id,
                (row_number() OVER (ORDER BY i.sales_rep_id, i.earned_on, d.number, i.id))::int
                    AS line_no,
                i.sales_rep_id, i.plan_id, d.doc_type AS source_type,
                d.number AS source_number, i.earned_on, i.basis, i.commission, i.dimension_set_id
         FROM commission_item i JOIN document d ON d.id = i.source_document_id
         WHERE i.document_id = $1 AND ($2::uuid IS NULL OR i.sales_rep_id = $2)
         ORDER BY i.sales_rep_id, i.earned_on, d.number, i.id",
    )
    .bind(id)
    .bind(rep)
    .fetch_all(&mut *conn)
    .await?)
}

async fn range(conn: &mut PgConnection, id: Uuid) -> Result<Option<(NaiveDate, NaiveDate)>> {
    Ok(
        sqlx::query_as("SELECT from_date, to_date FROM commission_run WHERE document_id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

// ------------------------------------------------------------ the run

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunBody {
    from_date: NaiveDate,
    to_date: NaiveDate,
    #[serde(default)]
    lines: Vec<LineInput>,
    /// Shown, never stored from here.
    #[serde(default)]
    #[allow(dead_code)]
    items: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct LineInput {
    id: Option<Uuid>,
    commission: Option<Decimal>,
}

pub(crate) struct Run;

#[async_trait]
impl DocumentHandler for Run {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: RUN.into(),
            module: String::new(),
            label: "Commission run".into(),
            plural: "Commission runs".into(),
            number_prefix: "CR".into(),
            fields: vec![
                FieldMeta::new("from_date", "From", FieldType::Date)
                    .required()
                    .help("The first day earned in the run."),
                FieldMeta::new("to_date", "To", FieldType::Date)
                    .required()
                    .help("The last day earned in the run."),
            ],
            collections: vec![
                CollectionMeta {
                    name: "lines".into(),
                    label: "Salespeople".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("sales_rep_id", "Salesperson", FieldType::link("sales_rep"))
                            .readonly(),
                        FieldMeta::new("plan_id", "Plan", FieldType::link("commission_plan"))
                            .readonly(),
                        money("basis", "Basis")
                            .readonly()
                            .help("What the salesperson's lines under the plan came to."),
                        money("proposed", "Proposed").readonly(),
                        money("commission", "Commission")
                            .help("What is accrued; the proposal unless changed here."),
                        FieldMeta::new("adjusted", "Adjusted", FieldType::Bool)
                            .readonly()
                            .help("The commission differs from what the plan says."),
                        FieldMeta::new("statement_id", "Statement", FieldType::document(STATEMENT))
                            .readonly(),
                    ],
                },
                CollectionMeta {
                    name: "items".into(),
                    label: "Items".into(),
                    fields: item_fields(),
                },
            ],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("posted", "Posted"),
                StateMeta::locked("reversed", "Reversed"),
                StateMeta::locked("cancelled", "Cancelled").unnumbered(),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "posted".into()),
                ("draft".into(), "cancelled".into()),
                ("posted".into(), "reversed".into()),
            ],
            summary: vec![
                FieldMeta::new("from_date", "From", FieldType::Date).readonly(),
                FieldMeta::new("to_date", "To", FieldType::Date).readonly(),
                FieldMeta::new("salespeople", "Salespeople", FieldType::Int).readonly(),
                money("total", "Commission").readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: RunBody = erpcraft_kernel::input::parse(body.clone(), "commission run body")?;
        if body.to_date < body.from_date {
            return Err(Error::field(
                "to_date",
                "the last day comes after the first",
            ));
        }
        let before = range(&mut *ctx.conn, doc.id).await?;
        sqlx::query(
            "INSERT INTO commission_run (document_id, from_date, to_date) VALUES ($1, $2, $3)
             ON CONFLICT (document_id) DO UPDATE SET from_date = EXCLUDED.from_date,
                 to_date = EXCLUDED.to_date",
        )
        .bind(doc.id)
        .bind(body.from_date)
        .bind(body.to_date)
        .execute(&mut *ctx.conn)
        .await?;
        // Another range makes the proposal stale: propose again.
        if before.is_some_and(|b| b != (body.from_date, body.to_date)) {
            clear(&mut *ctx.conn, doc.id).await?;
            return Ok(());
        }
        let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        for (index, line) in body.lines.iter().enumerate() {
            let (Some(id), Some(commission)) = (line.id, line.commission) else {
                continue;
            };
            if commission.normalize().scale() > decimals {
                return Err(Error::line(
                    "lines",
                    index + 1,
                    Some("commission"),
                    format!("has at most {decimals} decimal places"),
                ));
            }
            sqlx::query(
                "UPDATE commission_run_line SET commission = $3 WHERE id = $1 AND document_id = $2",
            )
            .bind(id)
            .bind(doc.id)
            .bind(commission)
            .execute(&mut *ctx.conn)
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
        let lines = document::body::lines(body, "lines");
        let reps: BTreeSet<String> = lines
            .iter()
            .map(|l| l["sales_rep_id"].to_string())
            .collect();
        Ok(json!({
            "from_date": body["from_date"],
            "to_date": body["to_date"],
            "salespeople": reps.len(),
            "total": lines.iter().map(|l| document::body::amount(&l["commission"])).sum::<Decimal>(),
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let range = range(conn, doc.id).await?;
        Ok(json!({
            "from_date": range.map(|r| r.0),
            "to_date": range.map(|r| r.1),
            "lines": run_lines(conn, doc.id, None).await?,
            "items": run_items(conn, doc.id, None).await?,
        }))
    }
}

async fn clear(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM commission_item WHERE document_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM commission_run_line WHERE document_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ------------------------------------------------------------ the proposal

/// A line of a sale, credit or payment that earns under a plan.
struct Earning {
    rep: Uuid,
    plan: Uuid,
    document_id: Uuid,
    posting_id: Uuid,
    line_id: Uuid,
    settlement_id: Option<Uuid>,
    earned_on: NaiveDate,
    basis: Decimal,
    dimension_set_id: Option<Uuid>,
}

/// What earned in `from..=to` and no other run that stands holds.
async fn earnings(
    ctx: &mut Ctx<'_>,
    run: &Document,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Earning>> {
    let company_id = run.company_id;
    let plans = plans(&mut *ctx.conn, company_id).await?;
    if plans.is_empty() {
        return Ok(vec![]);
    }
    let lines = erpcraft_analysis::sales_lines(ctx, company_id, from, to, None).await?;
    let docs: Vec<Uuid> = lines.iter().map(|l| l.document_id).collect();
    let types: HashMap<Uuid, String> = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, doc_type FROM document WHERE id = ANY ($1)",
    )
    .bind(&docs)
    .fetch_all(&mut *ctx.conn)
    .await?
    .into_iter()
    .collect();
    // Payments in the range of the open items invoices made: the share of
    // the item each one settled.
    let paid: Vec<(Uuid, NaiveDate, Uuid, Uuid, Option<Decimal>)> = sqlx::query_as(
        "SELECT s.id, sp.posting_date, ip.document_id, ip.id,
                s.amount / NULLIF(o.amount, 0)
         FROM settlement s
         JOIN posting sp ON sp.id = s.posting_id
         JOIN document sd ON sd.id = sp.document_id AND sd.doc_type = 'payment'
         JOIN open_item o ON o.id = s.open_item_id AND o.kind = 'receivable'
         JOIN posting ip ON ip.id = o.posting_id AND ip.reverses_id IS NULL
         JOIN document idoc ON idoc.id = ip.document_id AND idoc.doc_type = 'sales_invoice'
         WHERE s.company_id = $1 AND NOT s.cancels AND sp.posting_date BETWEEN $2 AND $3",
    )
    .bind(company_id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let paid_docs: Vec<Uuid> = paid.iter().map(|p| p.2).collect();
    let invoice_lines = if paid_docs.is_empty() {
        vec![]
    } else {
        erpcraft_analysis::sales_lines(
            ctx,
            company_id,
            NaiveDate::from_ymd_opt(1900, 1, 1).unwrap_or(from),
            NaiveDate::from_ymd_opt(9999, 12, 31).unwrap_or(to),
            Some(&paid_docs),
        )
        .await?
    };
    let items: BTreeSet<Uuid> = lines
        .iter()
        .chain(invoice_lines.iter())
        .filter_map(|l| l.item_id)
        .collect();
    let groups = erpcraft_analysis::groups_of_items(ctx, &items).await?;
    let basis = |plan: &Plan, l: &erpcraft_analysis::SalesLine| {
        if plan.basis == "margin" {
            l.net - l.cost
        } else {
            l.net
        }
    };
    let mut out = vec![];
    for l in &lines {
        let Some(rep) = l.salesperson_id else {
            continue;
        };
        let group = l.item_id.and_then(|i| groups.get(&i).copied());
        let Some(plan) = plan_for(&plans, rep, group, l.posting_date) else {
            continue;
        };
        let invoice = types
            .get(&l.document_id)
            .is_some_and(|t| t == "sales_invoice");
        // An invoice earns here under a plan on invoicing; a credit takes
        // back under either.
        if invoice && plan.trigger != "invoice" {
            continue;
        }
        out.push(Earning {
            rep,
            plan: plan.id,
            document_id: l.document_id,
            posting_id: l.posting_id,
            line_id: l.line_id,
            settlement_id: None,
            earned_on: l.posting_date,
            basis: basis(plan, l),
            dimension_set_id: l.dimension_set_id,
        });
    }
    for (settlement, day, invoice, posting, share) in &paid {
        let Some(share) = share else {
            continue;
        };
        for l in invoice_lines
            .iter()
            .filter(|l| l.document_id == *invoice && l.posting_id == *posting)
        {
            let Some(rep) = l.salesperson_id else {
                continue;
            };
            let group = l.item_id.and_then(|i| groups.get(&i).copied());
            let Some(plan) = plan_for(&plans, rep, group, *day) else {
                continue;
            };
            if plan.trigger != "payment" {
                continue;
            }
            out.push(Earning {
                rep,
                plan: plan.id,
                document_id: l.document_id,
                posting_id: l.posting_id,
                line_id: l.line_id,
                settlement_id: Some(*settlement),
                earned_on: *day,
                basis: (basis(plan, l) * share).round_dp(4),
                dimension_set_id: l.dimension_set_id,
            });
        }
    }
    // What another run that stands holds is not earned again.
    let held: Vec<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT i.source_posting_id, i.source_line_id, i.settlement_id
         FROM commission_item i JOIN document d ON d.id = i.document_id
         WHERE d.company_id = $1 AND d.id <> $2 AND d.status IN ('draft', 'posted')",
    )
    .bind(company_id)
    .bind(run.id)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let held: BTreeSet<(Uuid, Uuid, Option<Uuid>)> = held.into_iter().collect();
    out.retain(|e| !held.contains(&(e.posting_id, e.line_id, e.settlement_id)));
    Ok(out)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdInput {
    /// The commission run.
    id: Uuid,
}

pub(crate) struct Propose;

#[async_trait]
impl Command for Propose {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "commission_run.propose",
            "Work out what each salesperson earned from..to under the company's commission \
             plans: a line per salesperson and plan with the basis (net or margin), the \
             commission by the plan's tiers and an item per sale, credit or payment that earned \
             or took back. What another run that is not cancelled or reversed holds is left out. \
             Replaces an earlier proposal, commissions changed by hand included.",
        )
        .permission("commission_run:update")
        .from_states(&["draft"])
        .primary_when(PrimaryWhenSpec::states(&["draft"]).empty("lines"))
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RUN, input.id, "update").await?;
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} commission run cannot be proposed again",
                doc.status
            )));
        }
        ctx.require("sales_invoice:read", doc.company_id)?;
        let (from, to) = range(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::validation("the run has no range yet"))?;
        let earnings = earnings(ctx, &doc, from, to).await?;
        let plans = plans(&mut *ctx.conn, doc.company_id).await?;
        let decimals = company::currency_decimals(&mut *ctx.conn, doc.company_id).await?;
        clear(&mut *ctx.conn, doc.id).await?;
        let mut by: BTreeMap<(Uuid, Uuid), Vec<&Earning>> = BTreeMap::new();
        for e in &earnings {
            by.entry((e.rep, e.plan)).or_default().push(e);
        }
        for (n, ((rep, plan_id), list)) in by.iter().enumerate() {
            let plan = plans.iter().find(|p| p.id == *plan_id);
            let basis: Decimal = list.iter().map(|e| e.basis).sum();
            let commission = plan
                .map(|p| earned(&p.tiers, basis))
                .unwrap_or_default()
                .round_dp(decimals);
            sqlx::query(
                "INSERT INTO commission_run_line (id, document_id, line_no, sales_rep_id, plan_id,
                     basis, proposed, commission)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
            )
            .bind(new_id())
            .bind(doc.id)
            .bind(n as i32 + 1)
            .bind(rep)
            .bind(plan_id)
            .bind(basis)
            .bind(commission)
            .execute(&mut *ctx.conn)
            .await?;
            // Each item its share of the line's commission; what rounding
            // leaves goes to the largest.
            let shares = shares(commission, list.iter().map(|e| e.basis), decimals);
            for (e, share) in list.iter().zip(shares) {
                sqlx::query(
                    "INSERT INTO commission_item (id, document_id, sales_rep_id, plan_id,
                         source_document_id, source_posting_id, source_line_id, settlement_id,
                         earned_on, basis, commission, dimension_set_id)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                )
                .bind(new_id())
                .bind(doc.id)
                .bind(rep)
                .bind(plan_id)
                .bind(e.document_id)
                .bind(e.posting_id)
                .bind(e.line_id)
                .bind(e.settlement_id)
                .bind(e.earned_on)
                .bind(e.basis)
                .bind(share)
                .bind(e.dimension_set_id)
                .execute(&mut *ctx.conn)
                .await?;
            }
        }
        document::refresh_summary(ctx, &mut doc).await?;
        document::view(ctx, doc).await
    }
}

/// `total` shared out by `weights`, rounded to `decimals`; what rounding
/// leaves goes to the largest share.
fn shares(total: Decimal, weights: impl Iterator<Item = Decimal>, decimals: u32) -> Vec<Decimal> {
    let weights: Vec<Decimal> = weights.collect();
    let sum: Decimal = weights.iter().sum();
    if weights.is_empty() {
        return vec![];
    }
    let mut out: Vec<Decimal> = if sum.is_zero() {
        let mut v = vec![Decimal::ZERO; weights.len()];
        v[0] = total;
        v
    } else {
        weights
            .iter()
            .map(|w| (total * w / sum).round_dp(decimals))
            .collect()
    };
    let left = total - out.iter().sum::<Decimal>();
    if !left.is_zero() {
        let largest = (0..out.len())
            .rev()
            .max_by_key(|i| weights[*i].abs())
            .unwrap_or(0);
        out[largest] += left;
    }
    out
}

// ------------------------------------------------------------ posting

#[derive(sqlx::FromRow)]
struct Settings {
    expense_account_id: Uuid,
    payable_account_id: Uuid,
}

pub(crate) struct Post;

#[async_trait]
impl Command for Post {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "commission_run.post",
            "Post a reviewed commission run: the policy commission_run.post is asked (it may ask \
             for approval); the commission is accrued, commission expense debited with the \
             dimensions of the sales that earned it and commission payable credited, as an open \
             payable to the vendor of a salesperson paid as one; and a commission statement is \
             made for each salesperson. All or nothing.",
        )
        .permission("commission_run:post")
        .from_states(&["draft"])
        .primary_when(PrimaryWhenSpec::states(&["draft"]).not_empty("lines"))
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RUN, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} commission run cannot be posted",
                doc.status
            )));
        }
        let company_id = doc.company_id;
        let settings: Settings = sqlx::query_as(
            "SELECT expense_account_id, payable_account_id FROM commission_settings
             WHERE company_id = $1 FOR UPDATE",
        )
        .bind(company_id)
        .fetch_optional(&mut *ctx.conn)
        .await?
        .ok_or_else(|| {
            Error::validation(
                "the company has no commission settings: name its expense and payable accounts",
            )
        })?;
        let (from, to) = range(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::validation("the run has no range yet"))?;
        let lines = run_lines(&mut *ctx.conn, doc.id, None).await?;
        let items = run_items(&mut *ctx.conn, doc.id, None).await?;
        // Under the settings' lock: no item another run posted since.
        let taken: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM commission_item mine
             JOIN commission_item other
               ON other.source_posting_id = mine.source_posting_id
              AND other.source_line_id = mine.source_line_id
              AND other.settlement_id IS NOT DISTINCT FROM mine.settlement_id
              AND other.document_id <> mine.document_id
             JOIN document d ON d.id = other.document_id AND d.status = 'posted'
             WHERE mine.document_id = $1",
        )
        .bind(doc.id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        if taken > 0 {
            return Err(Error::state(
                "another run posted commission on lines of this one since it was proposed: \
                 propose it again",
            ));
        }
        let currency = company::base_currency(&mut *ctx.conn, company_id).await?;
        let total: Decimal = lines.iter().map(|l| l.commission).sum();
        let reps: BTreeSet<Uuid> = lines.iter().map(|l| l.sales_rep_id).collect();
        ctx.decide(
            POST,
            company_id,
            json!({ "total": total.to_string(), "currency": currency,
                    "salespeople": reps.len().to_string() }),
        )
        .await?;
        let decimals = company::currency_decimals(&mut *ctx.conn, company_id).await?;
        // The expense by dimension set: each line's commission shared over
        // its items as they were proposed.
        let mut expense: BTreeMap<Option<Uuid>, Decimal> = BTreeMap::new();
        for line in &lines {
            let mine: Vec<&Item> = items
                .iter()
                .filter(|i| i.sales_rep_id == line.sales_rep_id && i.plan_id == line.plan_id)
                .collect();
            let weights = mine.iter().map(|i| {
                if line.proposed.is_zero() {
                    i.basis
                } else {
                    i.commission
                }
            });
            for (item, share) in mine.iter().zip(shares(line.commission, weights, decimals)) {
                *expense.entry(item.dimension_set_id).or_default() += share;
            }
            if mine.is_empty() {
                *expense.entry(None).or_default() += line.commission;
            }
        }
        let payees: HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT sales_rep_id, vendor_id FROM commission_payee WHERE sales_rep_id = ANY ($1)",
        )
        .bind(reps.iter().copied().collect::<Vec<_>>())
        .fetch_all(&mut *ctx.conn)
        .await?
        .into_iter()
        .collect();
        let side = |amount: Decimal| {
            if amount >= Decimal::ZERO {
                (amount, Decimal::ZERO)
            } else {
                (Decimal::ZERO, -amount)
            }
        };
        if !total.is_zero() || lines.iter().any(|l| !l.commission.is_zero()) {
            let posting = document::create_posting(
                ctx,
                &doc,
                doc.doc_date,
                Some(format!("Commission {from} to {to}")),
            )
            .await?;
            let mut gl = vec![];
            for (set, amount) in &expense {
                if amount.is_zero() {
                    continue;
                }
                let (debit, credit) = side(*amount);
                gl.push(GlLine {
                    account_id: settings.expense_account_id,
                    debit,
                    credit,
                    dimensions: match set {
                        Some(set) => Dimensions::Exact(Some(*set)),
                        None => Dimensions::Defaults,
                    },
                    ..GlLine::default()
                });
            }
            let mut owed: BTreeMap<Uuid, Decimal> = BTreeMap::new();
            for line in &lines {
                *owed.entry(line.sales_rep_id).or_default() += line.commission;
            }
            for (rep, amount) in &owed {
                if amount.is_zero() {
                    continue;
                }
                let (debit, credit) = side(-*amount);
                let mut line = GlLine {
                    account_id: settings.payable_account_id,
                    debit,
                    credit,
                    ..GlLine::default()
                };
                if let Some(vendor) = payees.get(rep) {
                    line.scopes = vec![("vendor", *vendor)];
                }
                gl.push(line);
            }
            erpcraft_gl::post(ctx, &posting, &gl).await?;
            for (rep, amount) in &owed {
                if let (Some(vendor), false) = (payees.get(rep), amount.is_zero()) {
                    erpcraft_openitems::open(
                        &mut *ctx.conn,
                        &posting,
                        &NewOpenItem {
                            kind: Kind::Payable,
                            party_type: "vendor",
                            party_id: *vendor,
                            account_id: settings.payable_account_id,
                            amount: *amount,
                            due_date: Some(doc.doc_date),
                            terms: None,
                            foreign: None,
                        },
                    )
                    .await?;
                }
            }
        }
        // A statement per salesperson.
        for rep in &reps {
            let statement = document::create(
                ctx,
                document::NewDocument {
                    doc_type: STATEMENT.into(),
                    company_id,
                    doc_date: doc.doc_date,
                    memo: doc.number.as_ref().map(|n| format!("Commission run {n}")),
                    body: json!({ "run_id": doc.id, "sales_rep_id": rep }),
                },
            )
            .await?;
            let id = statement.header.id;
            let mut statement = document::lock(ctx, STATEMENT, id, "create").await?;
            document::assign_number(ctx, &mut statement).await?;
            document::transition(ctx, &mut statement, "issued").await?;
            sqlx::query(
                "UPDATE commission_run_line SET statement_id = $3
                 WHERE document_id = $1 AND sales_rep_id = $2",
            )
            .bind(doc.id)
            .bind(rep)
            .bind(id)
            .execute(&mut *ctx.conn)
            .await?;
            document::link(ctx, doc.id, id, "statement").await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

pub(crate) struct Reverse;

#[async_trait]
impl Command for Reverse {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "commission_run.reverse",
            "Take a posted commission run back: its accrual is reversed on the run's date, its \
             statements are cancelled, and what it held can be earned in another run.",
        )
        .permission("commission_run:reverse")
        .from_states(&["posted"])
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, RUN, input.id, "reverse").await?;
        if doc.status == "reversed" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "posted" {
            return Err(Error::state(format!(
                "a {} commission run cannot be reversed",
                doc.status
            )));
        }
        if let Ok(posting) = document::original_posting(&mut *ctx.conn, &doc).await {
            document::reverse_posting(ctx, posting.id, None, None).await?;
        }
        let statements: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT statement_id FROM commission_run_line
             WHERE document_id = $1 AND statement_id IS NOT NULL",
        )
        .bind(doc.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        for id in statements {
            let mut statement = document::lock(ctx, STATEMENT, id, "cancel").await?;
            if statement.status == "issued" {
                document::transition(ctx, &mut statement, "cancelled").await?;
            }
        }
        document::transition(ctx, &mut doc, "reversed").await?;
        document::view(ctx, doc).await
    }
}

pub(crate) struct Cancel;

#[async_trait]
impl Command for Cancel {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "commission_run.cancel",
            "Cancel a draft commission run: what it held can be earned in another run.",
        )
        .permission("commission_run:cancel")
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
                "a {} commission run cannot be cancelled",
                doc.status
            )));
        }
        document::transition(ctx, &mut doc, "cancelled").await?;
        document::view(ctx, doc).await
    }
}

// ------------------------------------------------------------ the statement

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatementBody {
    run_id: Uuid,
    sales_rep_id: Uuid,
}

pub(crate) struct Statement;

#[async_trait]
impl DocumentHandler for Statement {
    fn meta(&self) -> DocTypeMeta {
        DocTypeMeta {
            name: STATEMENT.into(),
            module: String::new(),
            label: "Commission statement".into(),
            plural: "Commission statements".into(),
            number_prefix: "CS".into(),
            fields: vec![
                FieldMeta::new("run_id", "Commission run", FieldType::document(RUN)).immutable(),
                FieldMeta::new("sales_rep_id", "Salesperson", FieldType::link("sales_rep"))
                    .immutable(),
                FieldMeta::new("vendor_id", "Paid as", FieldType::link("vendor")).readonly(),
                FieldMeta::new("from_date", "From", FieldType::Date).readonly(),
                FieldMeta::new("to_date", "To", FieldType::Date).readonly(),
                money("total", "Commission").readonly(),
            ],
            collections: vec![
                CollectionMeta {
                    name: "lines".into(),
                    label: "Plans".into(),
                    fields: vec![
                        FieldMeta::line_id(),
                        FieldMeta::line_no(),
                        FieldMeta::new("plan_id", "Plan", FieldType::link("commission_plan"))
                            .readonly(),
                        money("basis", "Basis").readonly(),
                        money("commission", "Commission").readonly(),
                    ],
                },
                CollectionMeta {
                    name: "items".into(),
                    label: "Items".into(),
                    fields: item_fields(),
                },
            ],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("issued", "Issued"),
                StateMeta::locked("cancelled", "Cancelled"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "issued".into()),
                ("issued".into(), "cancelled".into()),
            ],
            summary: vec![
                FieldMeta::new(
                    "salesperson_id",
                    "Salesperson",
                    FieldType::link("sales_rep"),
                )
                .readonly(),
                FieldMeta::new("party_type", "Party type", FieldType::Text).readonly(),
                FieldMeta::new(
                    "party_id",
                    "Party",
                    FieldType::entity_named_by("party_type"),
                )
                .readonly(),
                FieldMeta::new("from_date", "From", FieldType::Date).readonly(),
                FieldMeta::new("to_date", "To", FieldType::Date).readonly(),
                money("total", "Commission").readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        if ctx.command != "commission_run.post" {
            return Err(Error::validation(
                "a commission statement is made by posting a commission run",
            ));
        }
        let body: StatementBody =
            erpcraft_kernel::input::parse(body.clone(), "commission statement body")?;
        let (from, to) = range(&mut *ctx.conn, body.run_id)
            .await?
            .ok_or_else(|| Error::validation("the commission run has no range"))?;
        let total: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(sum(commission), 0) FROM commission_run_line
             WHERE document_id = $1 AND sales_rep_id = $2",
        )
        .bind(body.run_id)
        .bind(body.sales_rep_id)
        .fetch_one(&mut *ctx.conn)
        .await?;
        sqlx::query(
            "INSERT INTO commission_statement (document_id, run_id, sales_rep_id, vendor_id,
                 from_date, to_date, total)
             VALUES ($1, $2, $3,
                     (SELECT vendor_id FROM commission_payee WHERE sales_rep_id = $3), $4, $5, $6)",
        )
        .bind(doc.id)
        .bind(body.run_id)
        .bind(body.sales_rep_id)
        .bind(from)
        .bind(to)
        .bind(total)
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
        let vendor = &body["vendor_id"];
        Ok(json!({
            "salesperson_id": body["sales_rep_id"],
            "party_type": if vendor.is_null() { Value::Null } else { json!("vendor") },
            "party_id": vendor,
            "from_date": body["from_date"],
            "to_date": body["to_date"],
            "total": body["total"],
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        #[allow(clippy::type_complexity)]
        let header: Option<(Uuid, Uuid, Option<Uuid>, NaiveDate, NaiveDate, Decimal)> =
            sqlx::query_as(
                "SELECT run_id, sales_rep_id, vendor_id, from_date, to_date, total
                 FROM commission_statement WHERE document_id = $1",
            )
            .bind(doc.id)
            .fetch_optional(&mut *conn)
            .await?;
        let Some((run, rep, vendor, from, to, total)) = header else {
            return Ok(json!({ "lines": [], "items": [] }));
        };
        let lines: Vec<Value> = run_lines(conn, run, Some(rep))
            .await?
            .into_iter()
            .enumerate()
            .map(|(n, l)| {
                json!({ "id": l.id, "line_no": n + 1, "plan_id": l.plan_id, "basis": l.basis,
                        "commission": l.commission })
            })
            .collect();
        Ok(json!({
            "run_id": run,
            "sales_rep_id": rep,
            "vendor_id": vendor,
            "from_date": from,
            "to_date": to,
            "total": total,
            "lines": lines,
            "items": run_items(conn, run, Some(rep)).await?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_add_up_to_the_total() {
        let d = |n: i64, s: u32| Decimal::new(n, s);
        let out = shares(d(1000, 2), [d(1, 0), d(1, 0), d(1, 0)].into_iter(), 2);
        assert_eq!(out.iter().sum::<Decimal>(), d(1000, 2));
        assert_eq!(out, vec![d(334, 2), d(333, 2), d(333, 2)]);
        let none = shares(d(500, 2), [Decimal::ZERO, Decimal::ZERO].into_iter(), 2);
        assert_eq!(none, vec![d(500, 2), Decimal::ZERO]);
    }
}
