//! Payment runs: paying what is due to vendors in one batch.
//!
//! A run proposes the vendors' open items due by a day (or offering a
//! discount still open on the pay date), a person reviews and approves it
//! (the policy `payment_run.approve`), and posting it drafts and posts one
//! ordinary payment per vendor through the payments module's own
//! commands. From a posted run the module writes bank files (SEPA
//! pain.001, NACHA) and sends remittance advice by mail. Files are
//! generated, never sent.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::document::{self, Document, DocumentChange, DocumentHandler, DocumentView};
use erpcraft_kernel::{
    CollectionMeta, Command, CommandSpec, Ctx, Decimal, DocTypeMeta, DynCommand, EntityMeta, Error,
    FieldMeta, FieldType, JsonSchema, Migration, Module, PgConnection, PolicyPoint,
    PrimaryWhenSpec, Result, StateMeta, async_trait, command, company, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

mod files;

const PAYMENT_RUN: &str = "payment_run";
/// The policy point asked when a run is approved.
pub const APPROVE: &str = "payment_run.approve";

pub struct PaymentRunsModule;

impl Module for PaymentRunsModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![("de", include_str!("../i18n/de.json"))]
    }

    fn name(&self) -> &'static str {
        "payment_runs"
    }

    fn label(&self) -> String {
        "Payment runs".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["payments", "banking", "open_items", "mail"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "payment_runs",
            sql: include_str!("../migrations/0001_payment_runs.sql"),
        }]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![
            EntityMeta::new("payment_hold", "Payment hold")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor"))
                        .help("Hold everything owed to this vendor."),
                )
                .field(
                    FieldMeta::new(
                        "open_item_id",
                        "Open item",
                        FieldType::Lookup(
                            erpcraft_openitems::lookup().field("company_id", "company_id"),
                        ),
                    )
                    .help("Or hold this one item."),
                )
                .field(FieldMeta::new("reason", "Reason", FieldType::Text).required())
                .field(
                    FieldMeta::new("active", "Active", FieldType::Bool)
                        .help("Lift the hold by making it inactive."),
                )
                .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
                .order_by(&["created_at"])
                .search(&["reason"])
                .display(&["reason"])
                .company_field("company_id"),
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                APPROVE,
                "Asked when a payment run is approved: may this batch of vendor payments go \
                 ahead? Unless configured otherwise it may. A company that wants a second \
                 signature on large runs asks for approval, for example when \
                 `facts.total > 50000`.",
            )
            .number("total", "What the run pays out, in its currency")
            .as_money(Some("currency"))
            .number("count", "Payments the run makes: one per vendor")
            .number("lines", "Open items the run pays")
            .text("currency", "The run's currency")
            .refusal("this payment run is not approved"),
        ]
    }

    fn doc_types(&self) -> Vec<Arc<dyn DocumentHandler>> {
        vec![Arc::new(PaymentRun)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(Propose),
            command(Step {
                name: "payment_run.approve",
                description: "Approve a reviewed payment run, asking the policy \
                              payment_run.approve (facts: total, count of payments, lines, \
                              currency). The run is then locked, ready to post.",
                permission: "payment_run:approve",
                from: &["draft"],
                to: "approved",
                primary_when: Some(PrimaryWhenSpec::states(&["draft"]).not_empty("lines")),
            }),
            command(Step {
                name: "payment_run.cancel",
                description: "Cancel a payment run that is not posted: its items can be \
                              proposed again.",
                permission: "payment_run:cancel",
                from: &["draft", "approved"],
                to: "cancelled",
                primary_when: None,
            }),
            command(Post),
            command(files::BankFile),
            command(SendRemittances),
        ]
    }
}

// ------------------------------------------------------------ the run

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    bank_account_id: Uuid,
    due_by: NaiveDate,
    #[serde(default = "yes")]
    take_discounts: bool,
    #[serde(default)]
    lines: Vec<LineInput>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineInput {
    id: Option<Uuid>,
    open_item_id: Uuid,
    amount: Decimal,
    #[serde(default)]
    discount: Decimal,
    #[serde(default)]
    hold: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct Line {
    pub id: Uuid,
    pub line_no: i32,
    pub open_item_id: Uuid,
    pub vendor_id: Uuid,
    pub due_date: Option<NaiveDate>,
    pub discount_date: Option<NaiveDate>,
    pub amount: Decimal,
    pub discount: Decimal,
    pub hold: bool,
    pub payment_id: Option<Uuid>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Header {
    pub bank_account_id: Uuid,
    pub due_by: NaiveDate,
    pub currency: String,
    pub take_discounts: bool,
}

pub(crate) async fn load_header(conn: &mut PgConnection, id: Uuid) -> Result<Option<Header>> {
    Ok(sqlx::query_as(
        "SELECT bank_account_id, due_by, currency, take_discounts FROM payment_run
         WHERE document_id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?)
}

pub(crate) async fn load_lines(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Line>> {
    Ok(sqlx::query_as(
        "SELECT l.id, l.line_no, l.open_item_id, l.vendor_id, l.due_date, l.discount_date,
                l.amount, l.discount, l.hold, p.payment_id
         FROM payment_run_line l
         LEFT JOIN payment_run_payment p
             ON p.document_id = l.document_id AND p.vendor_id = l.vendor_id AND NOT l.hold
         WHERE l.document_id = $1 ORDER BY l.line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The currency a run pays in: its bank account's, else the accounting
/// currency.
async fn run_currency(conn: &mut PgConnection, company_id: Uuid, account: Uuid) -> Result<String> {
    let found: Option<(bool, Option<String>)> =
        sqlx::query_as("SELECT is_bank, currency FROM account WHERE id = $1 AND company_id = $2")
            .bind(account)
            .bind(company_id)
            .fetch_optional(&mut *conn)
            .await?;
    match found {
        Some((true, currency)) => match currency {
            Some(currency) => Ok(currency),
            None => company::base_currency(conn, company_id).await,
        },
        _ => Err(Error::validation(
            "the bank account must be a bank or cash account of the run's company",
        )
        .with_field("bank_account_id")),
    }
}

struct PaymentRun;

#[async_trait]
impl DocumentHandler for PaymentRun {
    fn meta(&self) -> DocTypeMeta {
        let money = |name: &str, label: &str| {
            FieldMeta::new(name, label, FieldType::Decimal)
                .money()
                .currency_in("currency")
        };
        DocTypeMeta {
            name: PAYMENT_RUN.into(),
            module: String::new(),
            label: "Payment run".into(),
            plural: "Payment runs".into(),
            number_prefix: "PR".into(),
            fields: vec![
                FieldMeta::new(
                    "bank_account_id",
                    "Bank account",
                    FieldType::link("account").filter("is_bank", "true"),
                )
                .required(),
                FieldMeta::new("due_by", "Due by", FieldType::Date)
                    .required()
                    .help("Items due on or before this day are proposed."),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly()
                .help("The bank account's currency, else the company's."),
                FieldMeta::new("take_discounts", "Take discounts", FieldType::Bool).help(
                    "Also propose items not yet due whose early-payment discount is still \
                     open on the pay date, and take the discount.",
                ),
            ],
            collections: vec![CollectionMeta {
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
                                .value("kind", "payable"),
                        ),
                    )
                    .required(),
                    FieldMeta::new("vendor_id", "Vendor", FieldType::link("vendor")).readonly(),
                    FieldMeta::new("due_date", "Due", FieldType::Date).readonly(),
                    FieldMeta::new("discount_date", "Discount until", FieldType::Date).readonly(),
                    money("amount", "Amount").required(),
                    money("discount", "Discount"),
                    FieldMeta::new("hold", "Hold", FieldType::Bool)
                        .help("Keep the item on the run but do not pay it."),
                    FieldMeta::new("payment_id", "Payment", FieldType::document("payment"))
                        .readonly(),
                ],
            }],
            states: vec![
                StateMeta::editable("draft", "Draft"),
                StateMeta::locked("approved", "Approved"),
                StateMeta::locked("posted", "Posted"),
                StateMeta::locked("cancelled", "Cancelled"),
            ],
            initial_state: "draft".into(),
            transitions: vec![
                ("draft".into(), "approved".into()),
                ("approved".into(), "posted".into()),
                ("draft".into(), "cancelled".into()),
                ("approved".into(), "cancelled".into()),
            ],
            summary: vec![
                FieldMeta::new(
                    "bank_account_id",
                    "Bank account",
                    FieldType::link("account"),
                )
                .readonly(),
                FieldMeta::new(
                    "currency",
                    "Currency",
                    FieldType::link_by("currency", "code"),
                )
                .readonly(),
                money("total", "Total")
                    .readonly()
                    .help("What the run pays out."),
                FieldMeta::new("vendors", "Vendors", FieldType::Int).readonly(),
            ],
            ..DocTypeMeta::default()
        }
    }

    async fn save_body(&self, ctx: &mut Ctx<'_>, doc: &Document, body: &Value) -> Result<()> {
        let body: Body = erpcraft_kernel::input::parse(body.clone(), "payment run body")?;
        let currency = run_currency(&mut *ctx.conn, doc.company_id, body.bank_account_id).await?;
        let ids: Vec<Uuid> = body.lines.iter().map(|l| l.open_item_id).collect();
        let items: Vec<(
            Uuid,
            String,
            String,
            Uuid,
            String,
            Option<NaiveDate>,
            Option<NaiveDate>,
        )> = sqlx::query_as(
            "SELECT id, kind, party_type, party_id, COALESCE(currency, $3), due_date,
                        discount_date
                 FROM open_item WHERE id = ANY ($1) AND company_id = $2",
        )
        .bind(&ids)
        .bind(doc.company_id)
        .bind(company::base_currency(&mut *ctx.conn, doc.company_id).await?)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut rows = vec![];
        for (index, line) in body.lines.iter().enumerate() {
            let n = index + 1;
            let fail = |field: &str, msg: &str| Error::line("lines", n, Some(field), msg);
            let Some(item) = items.iter().find(|i| i.0 == line.open_item_id) else {
                return Err(fail(
                    "open_item_id",
                    "the item must be an open item of the company",
                ));
            };
            if item.1 != "payable" || item.2 != "vendor" {
                return Err(fail(
                    "open_item_id",
                    "a payment run pays what is owed to vendors",
                ));
            }
            if item.4 != currency {
                return Err(fail(
                    "open_item_id",
                    &format!("the item is in {}, the run in {currency}", item.4),
                ));
            }
            if line.discount.is_sign_negative() {
                return Err(fail("discount", "a discount cannot be below zero"));
            }
            rows.push((line, item.3, item.5, item.6));
        }
        sqlx::query(
            "INSERT INTO payment_run (document_id, bank_account_id, due_by, currency, take_discounts)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (document_id) DO UPDATE SET bank_account_id = EXCLUDED.bank_account_id,
                 due_by = EXCLUDED.due_by, currency = EXCLUDED.currency,
                 take_discounts = EXCLUDED.take_discounts",
        )
        .bind(doc.id)
        .bind(body.bank_account_id)
        .bind(body.due_by)
        .bind(&currency)
        .bind(body.take_discounts)
        .execute(&mut *ctx.conn)
        .await?;
        let line_ids = document::line_ids(
            &mut *ctx.conn,
            "payment_run_line",
            doc.id,
            body.lines.iter().map(|l| l.id),
        )
        .await?;
        sqlx::query("DELETE FROM payment_run_line WHERE document_id = $1")
            .bind(doc.id)
            .execute(&mut *ctx.conn)
            .await?;
        for (n, ((line, vendor, due, discount_date), id)) in rows.iter().zip(&line_ids).enumerate()
        {
            sqlx::query(
                "INSERT INTO payment_run_line (id, document_id, line_no, open_item_id, vendor_id,
                     due_date, discount_date, amount, discount, hold)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(id)
            .bind(doc.id)
            .bind(n as i32 + 1)
            .bind(line.open_item_id)
            .bind(vendor)
            .bind(due)
            .bind(discount_date)
            .bind(line.amount)
            .bind(line.discount)
            .bind(line.hold)
            .execute(&mut *ctx.conn)
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
        Ok(())
    }

    async fn summary_of(
        &self,
        _conn: &mut PgConnection,
        _doc: &Document,
        body: &Value,
    ) -> Result<Value> {
        let lines = document::body::lines(body, "lines");
        let paid: Vec<&Value> = lines.iter().filter(|l| l["hold"] != json!(true)).collect();
        let total: Decimal = paid
            .iter()
            .map(|l| document::body::amount(&l["amount"]))
            .sum();
        let mut vendors: Vec<String> = paid.iter().map(|l| l["vendor_id"].to_string()).collect();
        vendors.sort();
        vendors.dedup();
        Ok(json!({
            "bank_account_id": body["bank_account_id"],
            "currency": body["currency"],
            "total": total,
            "vendors": vendors.len(),
        }))
    }

    async fn load_body(&self, conn: &mut PgConnection, doc: &Document) -> Result<Value> {
        let header = load_header(conn, doc.id).await?;
        Ok(json!({
            "bank_account_id": header.as_ref().map(|h| h.bank_account_id),
            "due_by": header.as_ref().map(|h| h.due_by),
            "currency": header.as_ref().map(|h| h.currency.clone()),
            "take_discounts": header.as_ref().map(|h| h.take_discounts),
            "lines": load_lines(conn, doc.id).await?,
        }))
    }
}

// ------------------------------------------------------------ commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdInput {
    /// Payment run id.
    id: Uuid,
}

/// Fills a draft run with what is due.
struct Propose;

#[async_trait]
impl Command for Propose {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment_run.propose",
            "Fill a draft payment run with what is due: every vendor open item in the run's \
             currency due by `due_by`, or (with take_discounts) offering an early-payment \
             discount still open on the pay date, which is then taken; net of the vendor's \
             credits in that currency. Held vendors and items (payment_hold), items on another \
             run that is not cancelled, and vendors whose items come to nothing or less are left \
             out. Replaces the run's lines; review them, then approve.",
        )
        .permission("payment_run:update")
        .from_states(&["draft"])
        .primary_when(PrimaryWhenSpec::states(&["draft"]).empty("lines"))
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let doc = document::lock(ctx, PAYMENT_RUN, input.id, "update").await?;
        if doc.status != "draft" {
            return Err(Error::state(format!(
                "a {} payment run cannot be proposed again",
                doc.status
            )));
        }
        ctx.require("open_item:read", doc.company_id)?;
        let header = load_header(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::validation("the run has no bank account yet"))?;
        let base = company::base_currency(&mut *ctx.conn, doc.company_id).await?;
        let candidates: Vec<Uuid> = sqlx::query_scalar(
            "SELECT i.id FROM open_item i
             WHERE i.company_id = $1 AND i.kind = 'payable' AND i.party_type = 'vendor'
               AND COALESCE(i.currency, $2) = $3
               AND COALESCE(i.txn_amount, i.amount) <> COALESCE(i.txn_settled, i.settled)
               AND NOT EXISTS (SELECT 1 FROM payment_hold h WHERE h.company_id = i.company_id
                               AND h.active AND (h.vendor_id = i.party_id OR h.open_item_id = i.id))
               AND NOT EXISTS (SELECT 1 FROM payment_run_line l JOIN document d ON d.id = l.document_id
                               WHERE l.open_item_id = i.id AND d.id <> $4
                                 AND d.status IN ('draft', 'approved'))
             ORDER BY i.party_id, i.due_date NULLS FIRST, i.id",
        )
        .bind(doc.company_id)
        .bind(&base)
        .bind(&header.currency)
        .bind(doc.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let items = erpcraft_openitems::lock(&mut *ctx.conn, doc.company_id, &candidates).await?;
        let pay_date = doc.doc_date;
        // Per vendor: its debts that are due (or worth paying now), and its
        // credits.
        let mut by_vendor: BTreeMap<Uuid, Vec<Value>> = BTreeMap::new();
        let mut net: BTreeMap<Uuid, Decimal> = BTreeMap::new();
        for item in &items {
            if item.txn_open.is_zero() {
                continue;
            }
            let credit = item.txn_open.is_sign_negative();
            let discount = item
                .discount_on(pay_date)
                .filter(|_| header.take_discounts && !credit)
                .map(|(_, d)| d)
                .filter(|d| *d < item.txn_open);
            let due = item.due_date.unwrap_or(item.doc_date) <= header.due_by;
            if !(credit || due || discount.is_some()) {
                continue;
            }
            let discount = discount.unwrap_or_default();
            let amount = item.txn_open - discount;
            *net.entry(item.party_id).or_default() += amount;
            by_vendor.entry(item.party_id).or_default().push(json!({
                "open_item_id": item.id,
                "amount": amount,
                "discount": discount,
            }));
        }
        let lines: Vec<Value> = by_vendor
            .into_iter()
            .filter(|(vendor, lines)| {
                net.get(vendor).is_some_and(|n| *n > Decimal::ZERO)
                    && lines
                        .iter()
                        .any(|l| document::body::amount(&l["amount"]) > Decimal::ZERO)
            })
            .flat_map(|(_, lines)| lines)
            .collect();
        document::update(
            ctx,
            DocumentChange {
                doc_type: PAYMENT_RUN.into(),
                id: doc.id,
                version: doc.version,
                doc_date: None,
                memo: None,
                body: Some(json!({
                    "bank_account_id": header.bank_account_id,
                    "due_by": header.due_by,
                    "take_discounts": header.take_discounts,
                    "lines": lines,
                })),
            },
        )
        .await
    }
}

/// The lines a run pays, by vendor.
fn by_vendor(lines: &[Line]) -> BTreeMap<Uuid, Vec<&Line>> {
    let mut out: BTreeMap<Uuid, Vec<&Line>> = BTreeMap::new();
    for line in lines.iter().filter(|l| !l.hold) {
        out.entry(line.vendor_id).or_default().push(line);
    }
    out
}

/// Refuses a run that would pay a vendor or item put on hold after it was
/// proposed: a line on hold (`hold`) is kept on the run but not paid.
async fn check_holds(conn: &mut PgConnection, company_id: Uuid, run: Uuid) -> Result<()> {
    let held: Option<(String, String)> = sqlx::query_as(
        "SELECT v.code, h.reason FROM payment_run_line l
         JOIN vendor v ON v.id = l.vendor_id
         JOIN payment_hold h ON h.company_id = $2 AND h.active
              AND (h.vendor_id = l.vendor_id OR h.open_item_id = l.open_item_id)
         WHERE l.document_id = $1 AND NOT l.hold
         ORDER BY l.line_no LIMIT 1",
    )
    .bind(run)
    .bind(company_id)
    .fetch_optional(&mut *conn)
    .await?;
    match held {
        Some((vendor, reason)) => Err(Error::validation(format!(
            "a payment to vendor {vendor} is on hold ({reason}): lift the hold, or set hold on \
             its lines"
        ))),
        None => Ok(()),
    }
}

/// Approving and cancelling.
struct Step {
    name: &'static str,
    description: &'static str,
    permission: &'static str,
    from: &'static [&'static str],
    to: &'static str,
    primary_when: Option<PrimaryWhenSpec>,
}

#[async_trait]
impl Command for Step {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        let mut spec = CommandSpec::action(self.name, self.description)
            .permission(self.permission)
            .from_states(self.from);
        if let Some(when) = self.primary_when {
            spec = spec.primary_when(when);
        }
        if self.to == "cancelled" {
            spec.destructive()
        } else {
            spec
        }
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let action = self.name.rsplit('.').next().unwrap_or_default();
        let mut doc = document::lock(ctx, PAYMENT_RUN, input.id, action).await?;
        if doc.status == self.to {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if !self.from.contains(&doc.status.as_str()) {
            return Err(Error::state(format!(
                "a {} payment run cannot be {}",
                doc.status,
                if self.to == "approved" {
                    "approved"
                } else {
                    "cancelled"
                }
            )));
        }
        if self.to == "approved" {
            let header = load_header(&mut *ctx.conn, doc.id)
                .await?
                .ok_or_else(|| Error::validation("the run has no bank account yet"))?;
            check_holds(&mut *ctx.conn, doc.company_id, doc.id).await?;
            let lines = load_lines(&mut *ctx.conn, doc.id).await?;
            let vendors = by_vendor(&lines);
            if vendors.is_empty() {
                return Err(Error::validation(
                    "the run pays nothing: propose it, or take items off hold",
                ));
            }
            let mut total = Decimal::ZERO;
            for lines in vendors.values() {
                let paid: Decimal = lines.iter().map(|l| l.amount).sum();
                if paid <= Decimal::ZERO {
                    return Err(Error::validation(
                        "a vendor's lines come to nothing or less: take them off the run",
                    ));
                }
                total += paid;
            }
            ctx.decide(
                APPROVE,
                doc.company_id,
                json!({
                    "total": total.to_string(),
                    "count": vendors.len(),
                    "lines": vendors.values().map(Vec::len).sum::<usize>(),
                    "currency": header.currency,
                }),
            )
            .await?;
        }
        document::transition(ctx, &mut doc, self.to).await?;
        document::view(ctx, doc).await
    }
}

/// Pays an approved run: one payment per vendor.
struct Post;

#[async_trait]
impl Command for Post {
    type Input = IdInput;
    type Output = DocumentView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment_run.post",
            "Pay an approved payment run: one outgoing payment per vendor through the run's bank \
             account, dated the run's date, with its lines' amounts and discounts, drafted and \
             posted with payment.post, so the payments module's rights, checks and policies \
             hold for each. All or nothing. Each payment is linked to the run.",
        )
        .permission("payment_run:post")
        .from_states(&["approved"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<DocumentView> {
        let mut doc = document::lock(ctx, PAYMENT_RUN, input.id, "post").await?;
        if doc.status == "posted" {
            ctx.no_change();
            return document::view(ctx, doc).await;
        }
        if doc.status != "approved" {
            return Err(Error::state(format!(
                "a {} payment run cannot be posted: approve it first",
                doc.status
            )));
        }
        let header = load_header(&mut *ctx.conn, doc.id)
            .await?
            .ok_or_else(|| Error::internal("a payment run without its header"))?;
        check_holds(&mut *ctx.conn, doc.company_id, doc.id).await?;
        let lines = load_lines(&mut *ctx.conn, doc.id).await?;
        let post = ctx
            .kernel
            .registry()
            .command("payment.post")?
            .handler
            .clone();
        for (vendor, lines) in by_vendor(&lines) {
            let allocations: Vec<Value> = lines
                .iter()
                .map(|l| {
                    json!({ "open_item_id": l.open_item_id, "amount": l.amount,
                            "discount": l.discount })
                })
                .collect();
            let draft = document::create(
                ctx,
                document::NewDocument {
                    doc_type: "payment".into(),
                    company_id: doc.company_id,
                    doc_date: doc.doc_date,
                    memo: doc.number.as_ref().map(|n| format!("Payment run {n}")),
                    body: json!({
                        "direction": "outgoing",
                        "bank_account_id": header.bank_account_id,
                        "currency": header.currency,
                        "allocations": allocations,
                    }),
                },
            )
            .await?;
            let payment = draft.header.id;
            post.run_json(ctx, json!({ "id": payment })).await?;
            document::link(ctx, doc.id, payment, "payment").await?;
            sqlx::query(
                "INSERT INTO payment_run_payment (document_id, vendor_id, payment_id)
                 VALUES ($1, $2, $3)",
            )
            .bind(doc.id)
            .bind(vendor)
            .bind(payment)
            .execute(&mut *ctx.conn)
            .await?;
        }
        document::transition(ctx, &mut doc, "posted").await?;
        document::view(ctx, doc).await
    }
}

/// The payments a posted run made that still stand, in order: one
/// reversed since paid nothing.
pub(crate) async fn payments_of(conn: &mut PgConnection, run: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar(
        "SELECT p.payment_id FROM payment_run_payment p
         JOIN payment_run_line l ON l.document_id = p.document_id AND l.vendor_id = p.vendor_id
         JOIN document d ON d.id = p.payment_id
         WHERE p.document_id = $1 AND d.status = 'posted'
         GROUP BY p.payment_id ORDER BY min(l.line_no)",
    )
    .bind(run)
    .fetch_all(&mut *conn)
    .await?)
}

#[derive(Serialize, JsonSchema)]
struct Sent {
    payment_id: Uuid,
    /// The message recorded on the payment, when it was sent.
    message_id: Option<Uuid>,
    /// Why it was not sent, such as a vendor without an email address.
    skipped: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct SentOutput {
    id: Uuid,
    sent: Vec<Sent>,
}

/// Emails each payment of a run to its vendor.
struct SendRemittances;

#[async_trait]
impl Command for SendRemittances {
    type Input = IdInput;
    type Output = SentOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "payment_run.send_remittances",
            "Send the remittance advice of a posted payment run: each payment by email to its \
             vendor with document.send (the payment's PDF lists the invoices it pays), to the \
             contact email the vendor has on record. A vendor without one is listed as skipped; \
             a payment reversed since is not sent. \
             Takes the right to send documents.",
        )
        .permission("payment_run:read")
        .from_states(&["posted"])
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: IdInput) -> Result<SentOutput> {
        // Read, not locked: sending changes nothing on the run.
        let doc = document::get(ctx, PAYMENT_RUN, input.id).await?.header;
        if doc.status != "posted" {
            return Err(Error::state(
                "remittance advice is sent for a posted payment run",
            ));
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
        let mut sent = vec![];
        for payment in payments_of(&mut *ctx.conn, doc.id).await? {
            let party = parties
                .run_json(ctx, json!({ "doc_type": "payment", "id": payment }))
                .await?;
            let seller = &party["seller"];
            let address = ["contact_email", "electronic_address"]
                .iter()
                .find_map(|f| seller[f].as_str().filter(|a| a.contains('@')));
            if address.is_none() {
                sent.push(Sent {
                    payment_id: payment,
                    message_id: None,
                    skipped: Some("the vendor has no email address on record".into()),
                });
                continue;
            }
            let message = send
                .run_json(
                    ctx,
                    json!({ "doc_type": "payment", "id": payment,
                            "subject": format!("Remittance advice {}", doc.number.clone().unwrap_or_default()) }),
                )
                .await?;
            sent.push(Sent {
                payment_id: payment,
                message_id: document::body::uuid(&message["id"]),
                skipped: None,
            });
        }
        Ok(SentOutput { id: doc.id, sent })
    }
}
