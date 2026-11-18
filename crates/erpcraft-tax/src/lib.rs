//! Tax, as far as every country needs it.
//!
//! A tax code is a rate. A company says which accounts each code posts
//! to. A module that posts an invoice asks [`quote`] what the tax on a net
//! amount is and which account it goes to, posts that to the general
//! ledger itself, and records it with [`record`] under the same posting.
//! The record is what a tax return is built from: [`tax.report`] totals it
//! by code.
//!
//! Tax is worked out per line and rounded per line, to the decimals of the
//! company's accounting currency.
//!
//! What is deliberately not here: which code applies to which customer,
//! item or place, rates that change over time, and the form of any
//! country's return. Those are for country modules, which can build on
//! the codes and on the record kept here.
//!
//! [`tax.report`]: Report

pub mod lines;

use std::sync::Arc;

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Decision, DynCommand, EntityMeta, Error, FieldMeta,
    FieldType, JsonSchema, Ledger, Migration, Module, PgConnection, PolicyPoint, Posting, Result,
    async_trait, command, company, new_id, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

pub struct TaxModule;

impl Module for TaxModule {
    fn name(&self) -> &'static str {
        "tax"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "tax",
                sql: include_str!("../migrations/0001_tax.sql"),
            },
            Migration {
                version: 2,
                name: "credits",
                sql: include_str!("../migrations/0002_credits.sql"),
            },
            Migration {
                version: 3,
                name: "groups",
                sql: include_str!("../migrations/0003_groups.sql"),
            },
            Migration {
                version: 4,
                name: "currency",
                sql: include_str!("../migrations/0004_currency.sql"),
            },
            Migration {
                version: 5,
                name: "entry_account",
                sql: include_str!("../migrations/0005_entry_account.sql"),
            },
            Migration {
                version: 6,
                name: "charge",
                sql: include_str!("../migrations/0006_charge.sql"),
            },
            Migration {
                version: 7,
                name: "self_assessed",
                sql: include_str!("../migrations/0007_self_assessed.sql"),
            },
            Migration {
                version: 8,
                name: "tax_rule",
                sql: include_str!("../migrations/0008_tax_rule.sql"),
            },
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                USE_CODE,
                "Asked for every invoice line that carries a tax code, when the invoice is \
                 posted: may the line be taxed with this code? For a group (a code made of \
                 components, such as GST and PST) it is asked for the group and for each \
                 component. Unless configured otherwise an inactive code is refused and an \
                 active one accepted. A company that still has \
                 to invoice at a retired rate allows it, for example for one code with \
                 `facts.code == 'VAT19'`, without making the code active for new work.",
            )
            .text("code", "The tax code")
            .number("rate", "Its rate in percent")
            .as_percent()
            .boolean("active", "Whether the code is active")
            .text(
                "direction",
                "collected (tax on a sale) or paid (tax a vendor charges)",
            )
            .refuse_when("!facts.active")
            .refusal("the tax code is inactive"),
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("tax_code", "Tax code")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("rate", "Rate in percent", FieldType::Decimal)
                        .percent(6)
                        .required(),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(
                    FieldMeta::new("compound", "Compounding", FieldType::Bool).help(
                        "As a component of a group: charged on the net amount plus the taxes \
                         of the components before it.",
                    ),
                )
                .field(
                    FieldMeta::new("authority", "Authority", FieldType::Text)
                        .help("Who the tax is owed to."),
                )
                .field(
                    FieldMeta::new(
                        "self_assessed",
                        "Accounted for by the buyer",
                        FieldType::Bool,
                    )
                    .help(
                        "Reverse charge, an intra-Community acquisition or US use tax. On \
                             a purchase the vendor charges none: the company owes the tax on \
                             the collected account and reclaims it on the paid account (an \
                             expense where it cannot). On a sale nothing is charged and the \
                             base is recorded.",
                    ),
                )
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("tax_rule", "Tax rule")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new(
                        "scope_type",
                        "For",
                        FieldType::labelled(&[
                            ("customer", "Customer"),
                            ("vendor", "Vendor"),
                            ("item", "Item"),
                        ]),
                    )
                    .help("Empty: the company's default code."),
                )
                .field(FieldMeta::new(
                    "scope_id",
                    "Record",
                    FieldType::entity_named_by("scope_type"),
                ))
                .field(
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                        .required()
                        .help(
                            "What an invoice line starts with when it names no code: the \
                             party's rule first, then the item's, then the charge's own code, \
                             then the company's default.",
                        ),
                )
                .field(created())
                .order_by(&["scope_type", "scope_id"])
                .written_together(&["scope_type", "scope_id"])
                .company_field("company_id"),
            EntityMeta::new("tax_code_component", "Tax group component")
                .field(
                    FieldMeta::new("group_id", "Group", FieldType::link("tax_code"))
                        .required()
                        .immutable()
                        .help("The code a line carries, made of components."),
                )
                .field(
                    FieldMeta::new("component_id", "Component", FieldType::link("tax_code"))
                        .required()
                        .help("A plain code: its own rate and accounts."),
                )
                .field(
                    FieldMeta::new("position", "Position", FieldType::Int)
                        .required()
                        .help("The order the taxes are worked out in, from 1."),
                )
                .field(created())
                .order_by(&["group_id", "position"]),
            EntityMeta::new("charge", "Charge")
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(FieldMeta::new(
                    "kind",
                    "Kind",
                    FieldType::options(&["freight", "insurance", "handling", "service", "other"]),
                ))
                .field(
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                        .help("The tax code a line of this charge starts with."),
                )
                .field(
                    FieldMeta::new("landed_cost", "Landed cost", FieldType::Bool).help(
                        "On a vendor invoice, a cost of the goods received, allocated to them, \
                         rather than an expense.",
                    ),
                )
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"]),
            EntityMeta::new("tax_settings", "Tax accounts")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("tax_code_id", "Tax code", FieldType::link("tax_code"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new(
                        "collected_account_id",
                        "Account for tax collected",
                        FieldType::link("account"),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new(
                        "paid_account_id",
                        "Account for tax paid",
                        FieldType::link("account"),
                    )
                    .required(),
                )
                .field(created())
                .company_field("company_id"),
        ]
    }

    fn ledgers(&self) -> Vec<Arc<dyn Ledger>> {
        vec![Arc::new(TaxLedger)]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Report)]
    }
}

/// Who charged the tax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// The company charged it to a customer and owes it to the authority.
    Collected,
    /// A vendor charged it to the company.
    Paid,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Collected => "collected",
            Direction::Paid => "paid",
        }
    }
}

/// The tax on one net amount, ready to post and to record.
#[derive(Debug, Clone)]
pub struct Tax {
    pub tax_code_id: Uuid,
    pub direction: Direction,
    /// Percent, as the code had it when this was worked out.
    pub rate: Decimal,
    /// The net amount taxed.
    pub base: Decimal,
    /// The tax: `base * rate / 100`, rounded to the currency's decimals.
    pub amount: Decimal,
    /// The general ledger account the tax goes to: credit it for tax
    /// collected, debit it for tax paid.
    pub account_id: Uuid,
    /// The document line taxed.
    pub source_line_id: Option<Uuid>,
    /// For a credit: the tax entry of the invoice it takes tax back from.
    /// `base` and `amount` are then below zero.
    pub credits_id: Option<Uuid>,
    /// The code the line carries when it is a group and this is one of
    /// its components.
    pub group_code_id: Option<Uuid>,
    /// Charged on the net amount plus the taxes before it in the group:
    /// `base` includes them.
    pub compound: bool,
    /// For a document in a foreign currency: `base` and `amount` are in
    /// that currency, and this is what they are worth in the accounting
    /// currency, which the tax accounts hold and a return totals.
    pub foreign: Option<Converted>,
    /// Tax the buyer accounts for itself (reverse charge, use tax): not
    /// charged by the party and not owed with the invoice. On a purchase a
    /// code gives two: `paid` (debit its account) and `collected` (credit
    /// its account), of the same amount. On a sale it is for nothing.
    pub self_assessed: bool,
}

/// The accounting-currency value of a tax worked out in another currency.
#[derive(Debug, Clone)]
pub struct Converted {
    pub currency: String,
    pub base: Decimal,
    pub amount: Decimal,
}

/// The currency of a document and the rate it posts at: accounting
/// currency per unit.
#[derive(Debug, Clone)]
pub struct Conversion {
    pub currency: String,
    pub rate: Decimal,
}

impl Tax {
    /// The tax of several components together that the party charges, in
    /// the document's currency: what is owed with the invoice. Tax the
    /// buyer accounts for itself is not.
    pub fn total(taxes: &[Tax]) -> Decimal {
        taxes
            .iter()
            .filter(|tax| !tax.self_assessed)
            .map(|tax| tax.amount)
            .sum()
    }

    /// The general ledger line for this tax, in the accounting currency:
    /// debit the account for tax paid, credit it for tax collected, the
    /// other way round for tax taken back (an amount below zero).
    pub fn debit_credit(&self) -> (Decimal, Decimal) {
        let value = self.value();
        match (self.direction, value.is_sign_negative()) {
            (Direction::Paid, false) => (value, Decimal::ZERO),
            (Direction::Paid, true) => (Decimal::ZERO, -value),
            (Direction::Collected, false) => (Decimal::ZERO, value),
            (Direction::Collected, true) => (-value, Decimal::ZERO),
        }
    }

    /// What the tax is worth in the accounting currency.
    pub fn value(&self) -> Decimal {
        self.foreign.as_ref().map_or(self.amount, |f| f.amount)
    }
}

/// The net amount of a document line, which tax is worked out on: its
/// quantity times its unit price, rounded to `decimals`, less a percent
/// discount (rounded the same way) and an amount discount. Each line is
/// rounded on its own, so a document's total is the sum of its lines.
/// `None` when the discounts come to more than the line.
pub fn line_net(
    qty: Decimal,
    unit_price: Decimal,
    discount_percent: Option<Decimal>,
    discount_amount: Option<Decimal>,
    decimals: u32,
) -> Option<Decimal> {
    let gross = round_money(qty * unit_price, decimals);
    let percent = discount_percent
        .map(|p| round_money(gross * p / Decimal::ONE_HUNDRED, decimals))
        .unwrap_or_default();
    let net = gross - percent - discount_amount.unwrap_or_default();
    (net >= Decimal::ZERO).then_some(net)
}

/// The tax code an invoice line of `company_id` starts with when it names
/// none: the rule for its party (`("customer", id)` or `("vendor", id)`),
/// else the rule for its item, else its charge's own code, else the
/// company's default rule. `None` when nothing says.
pub async fn proposed_code(
    conn: &mut PgConnection,
    company_id: Uuid,
    party: (&str, Uuid),
    item_id: Option<Uuid>,
    charge_id: Option<Uuid>,
) -> Result<Option<Uuid>> {
    let rules: Vec<(Option<String>, Option<Uuid>, Uuid)> = sqlx::query_as(
        "SELECT scope_type, scope_id, tax_code_id FROM tax_rule
         WHERE company_id = $1
           AND (scope_type IS NULL
                OR (scope_type = $2 AND scope_id = $3)
                OR (scope_type = 'item' AND scope_id = $4))",
    )
    .bind(company_id)
    .bind(party.0)
    .bind(party.1)
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    let rule = |scope: Option<&str>| {
        rules
            .iter()
            .find(|(t, ..)| t.as_deref() == scope)
            .map(|(.., code)| *code)
    };
    if let Some(code) = rule(Some(party.0)) {
        return Ok(Some(code));
    }
    if item_id.is_some()
        && let Some(code) = rule(Some("item"))
    {
        return Ok(Some(code));
    }
    if let Some(charge) = charge_id {
        let own: Option<Uuid> = sqlx::query_scalar("SELECT tax_code_id FROM charge WHERE id = $1")
            .bind(charge)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
        if own.is_some() {
            return Ok(own);
        }
    }
    Ok(rule(None))
}

/// The policy point asked by [`quote`]: may an invoice line be taxed with
/// this code?
pub const USE_CODE: &str = "tax_code.use";

/// One tax a code stands for: the code itself, or a component of a group.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Part {
    id: Uuid,
    code: String,
    rate: Decimal,
    active: bool,
    compound: bool,
    self_assessed: bool,
}

/// The code a line carries, and the taxes it is made of, in order: the
/// code alone when it has no components.
async fn parts(conn: &mut PgConnection, tax_code_id: Uuid) -> Result<(Part, Vec<Part>)> {
    let code: Option<Part> = sqlx::query_as(
        "SELECT id, code, rate, active, compound, self_assessed FROM tax_code WHERE id = $1",
    )
    .bind(tax_code_id)
    .fetch_optional(&mut *conn)
    .await?;
    let code = code.ok_or_else(|| Error::not_found("tax code"))?;
    let components: Vec<Part> = sqlx::query_as(
        "SELECT c.id, c.code, c.rate, c.active, c.compound, c.self_assessed
         FROM tax_code_component g
         JOIN tax_code c ON c.id = g.component_id
         WHERE g.group_id = $1 ORDER BY g.position",
    )
    .bind(tax_code_id)
    .fetch_all(&mut *conn)
    .await?;
    let components = if components.is_empty() {
        vec![code.clone()]
    } else {
        components
    };
    Ok((code, components))
}

/// Work out the tax on `base` for a company with the code a line carries,
/// and where it posts, for a document that is being posted: one [`Tax`]
/// per component, in the group's order, or one for a plain code. Changes
/// nothing. Asks the company's policy [`USE_CODE`] whether the code, and
/// each component of a group, may be used (by default: only an active
/// one), and fails if the company has not said which accounts a component
/// posts to.
///
/// For a document in a foreign currency (`conversion`), `base` and the tax
/// are in that currency, rounded to its decimals, and each tax also says
/// what it is worth in the accounting currency at the document's rate.
pub async fn quote(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    tax_code_id: Uuid,
    direction: Direction,
    base: Decimal,
    source_line_id: Option<Uuid>,
    conversion: Option<&Conversion>,
) -> Result<Vec<Tax>> {
    let (code, components) = parts(&mut *ctx.conn, tax_code_id).await?;
    let taxes = work_out(
        &mut *ctx.conn,
        company_id,
        &code,
        &components,
        direction,
        base,
        source_line_id,
        conversion,
    )
    .await?;
    let group = components[0].id != code.id;
    let policy = ctx.policy(USE_CODE, company_id).await?;
    let mut asked = vec![(&code, code.rate)];
    if group {
        // A group is asked at the rate of its parts together.
        asked[0].1 = components.iter().map(|c| c.rate).sum();
        asked.extend(components.iter().map(|c| (c, c.rate)));
    }
    for (part, rate) in asked {
        let facts = json!({
            "code": part.code,
            "rate": rate.normalize().to_string(),
            "active": part.active,
            "direction": direction.as_str(),
        });
        if let Decision::Refused(refusal) = policy.ruling(facts)?.decision {
            return Err(Error::validation(
                refusal
                    .message
                    .unwrap_or_else(|| format!("tax code {} is inactive", part.code)),
            ));
        }
    }
    Ok(taxes)
}

/// What [`quote`] would say, without asking the policy: for showing the
/// tax on a draft. A draft taxed with a code its company does not accept
/// is refused when it is posted.
pub async fn estimate(
    conn: &mut PgConnection,
    company_id: Uuid,
    tax_code_id: Uuid,
    direction: Direction,
    base: Decimal,
    source_line_id: Option<Uuid>,
    conversion: Option<&Conversion>,
) -> Result<Vec<Tax>> {
    let (code, components) = parts(conn, tax_code_id).await?;
    work_out(
        conn,
        company_id,
        &code,
        &components,
        direction,
        base,
        source_line_id,
        conversion,
    )
    .await
}

/// The tax of each component on `base`, rounded per component: a
/// compounding component of a group is charged on `base` plus the taxes
/// before it.
#[allow(clippy::too_many_arguments)]
async fn work_out(
    conn: &mut PgConnection,
    company_id: Uuid,
    code: &Part,
    components: &[Part],
    direction: Direction,
    base: Decimal,
    source_line_id: Option<Uuid>,
    conversion: Option<&Conversion>,
) -> Result<Vec<Tax>> {
    let ids: Vec<Uuid> = components.iter().map(|c| c.id).collect();
    let accounts: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT tax_code_id, collected_account_id, paid_account_id FROM tax_settings
         WHERE company_id = $1 AND tax_code_id = ANY($2)",
    )
    .bind(company_id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let base_decimals = company::currency_decimals(conn, company_id).await?;
    let decimals = match conversion {
        Some(conversion) => {
            let found: Option<i16> =
                sqlx::query_scalar("SELECT decimals FROM currency WHERE code = $1")
                    .bind(&conversion.currency)
                    .fetch_optional(&mut *conn)
                    .await?;
            found.map_or(base_decimals, |d| d as u32)
        }
        None => base_decimals,
    };
    let group = (components[0].id != code.id).then_some(code.id);
    let mut taxes: Vec<Tax> = Vec::with_capacity(components.len());
    for part in components {
        let (_, collected, paid) =
            accounts
                .iter()
                .find(|(id, ..)| *id == part.id)
                .ok_or_else(|| {
                    Error::validation(format!(
                        "this company has not said which accounts tax code {} posts to",
                        part.code
                    ))
                })?;
        let compound = part.compound && group.is_some();
        let taxed = if compound {
            base + Tax::total(&taxes)
        } else {
            base
        };
        // A sale with tax the buyer accounts for charges none.
        let amount = if part.self_assessed && direction == Direction::Collected {
            Decimal::ZERO
        } else {
            round_money(taxed * part.rate / Decimal::ONE_HUNDRED, decimals)
        };
        taxes.push(Tax {
            tax_code_id: part.id,
            direction,
            rate: part.rate,
            base: taxed,
            amount,
            account_id: match direction {
                Direction::Collected => *collected,
                Direction::Paid => *paid,
            },
            source_line_id,
            credits_id: None,
            group_code_id: group,
            compound,
            foreign: conversion.map(|c| Converted {
                currency: c.currency.clone(),
                base: round_money(taxed * c.rate, base_decimals),
                amount: round_money(amount * c.rate, base_decimals),
            }),
            self_assessed: part.self_assessed,
        });
        // On a purchase the company owes what it reclaims: the same tax
        // again, collected.
        if part.self_assessed && direction == Direction::Paid {
            let mut owed = taxes.last().cloned().expect("just pushed");
            owed.direction = Direction::Collected;
            owed.account_id = *collected;
            taxes.push(owed);
        }
    }
    Ok(taxes)
}

/// The tax to take back for crediting `base` (more than zero: the net
/// amount credited) of the invoice line `invoice_line_id`: for each tax the
/// invoice recorded on that line (each component of a group), at the code,
/// direction and rate it recorded, whatever the code says now, as negative
/// amounts. A compounding component is taken back on the amount credited
/// plus the taxes taken back before it. The last of an entry's base takes
/// the last of its tax, so that crediting a line in parts takes back
/// exactly what it charged. Empty if the invoice recorded no tax on that
/// line. Changes nothing; record it with [`record`].
///
/// For an invoice in a foreign currency `base` and the tax are in its
/// currency, and `rate` is the rate the credit posts at; the last of an
/// entry also takes the last of its accounting value.
pub async fn credit(
    conn: &mut PgConnection,
    company_id: Uuid,
    invoice_line_id: Uuid,
    base: Decimal,
    source_line_id: Option<Uuid>,
    rate: Decimal,
) -> Result<Vec<Tax>> {
    #[derive(sqlx::FromRow)]
    struct Charged {
        id: Uuid,
        tax_code_id: Uuid,
        direction: String,
        rate: Decimal,
        base: Decimal,
        amount: Decimal,
        group_code_id: Option<Uuid>,
        compound: bool,
        self_assessed: bool,
        taken_base: Decimal,
        taken: Decimal,
        collected_account_id: Option<Uuid>,
        paid_account_id: Option<Uuid>,
        currency: Option<String>,
        txn_base: Decimal,
        txn_amount: Decimal,
        txn_taken_base: Decimal,
        txn_taken: Decimal,
    }
    // The line's taxes on the invoice's posting that still stands, in the
    // order they were worked out.
    let charged: Vec<Charged> = sqlx::query_as(
        "SELECT e.id, e.tax_code_id, e.direction, e.rate, e.base, e.amount, e.group_code_id,
                e.compound, e.self_assessed,
                COALESCE((SELECT sum(c.base) FROM tax_entry c WHERE c.credits_id = e.id), 0)
                    AS taken_base,
                COALESCE((SELECT sum(c.amount) FROM tax_entry c WHERE c.credits_id = e.id), 0)
                    AS taken,
                -- The account the invoice's tax was posted to, whatever the
                -- settings name now.
                COALESCE(e.account_id, s.collected_account_id) AS collected_account_id,
                COALESCE(e.account_id, s.paid_account_id) AS paid_account_id,
                e.currency, COALESCE(e.txn_base, e.base) AS txn_base,
                COALESCE(e.txn_amount, e.amount) AS txn_amount,
                COALESCE((SELECT sum(COALESCE(c.txn_base, c.base)) FROM tax_entry c
                          WHERE c.credits_id = e.id), 0) AS txn_taken_base,
                COALESCE((SELECT sum(COALESCE(c.txn_amount, c.amount)) FROM tax_entry c
                          WHERE c.credits_id = e.id), 0) AS txn_taken
         FROM tax_entry e
         LEFT JOIN tax_settings s ON s.company_id = e.company_id AND s.tax_code_id = e.tax_code_id
         WHERE e.source_line_id = $1 AND e.company_id = $2 AND e.reverses_id IS NULL
           AND e.credits_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM tax_entry r WHERE r.reverses_id = e.id)
         ORDER BY e.created_at, e.line_no",
    )
    .bind(invoice_line_id)
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    let base_decimals = company::currency_decimals(conn, company_id).await?;
    let mut taxes: Vec<Tax> = Vec::with_capacity(charged.len());
    for entry in charged {
        let decimals = match &entry.currency {
            Some(currency) => {
                let found: Option<i16> =
                    sqlx::query_scalar("SELECT decimals FROM currency WHERE code = $1")
                        .bind(currency)
                        .fetch_optional(&mut *conn)
                        .await?;
                found.map_or(base_decimals, |d| d as u32)
            }
            None => base_decimals,
        };
        // What is credited of this tax's own base: the amount credited,
        // and for a compounding tax the taxes taken back before it.
        let credited = if entry.compound {
            base - Tax::total(&taxes)
        } else {
            base
        };
        let left_base = entry.txn_base + entry.txn_taken_base;
        let left = entry.txn_amount + entry.txn_taken;
        let last = credited >= left_base;
        let amount = if last {
            left
        } else {
            round_money(credited * entry.rate / Decimal::ONE_HUNDRED, decimals).min(left)
        };
        // In the accounting currency: at the credit's rate, and the last
        // of the entry takes the last of its value.
        let foreign = entry.currency.clone().map(|currency| {
            let left_value_base = entry.base + entry.taken_base;
            let left_value = entry.amount + entry.taken;
            Converted {
                currency,
                base: if last {
                    left_value_base
                } else {
                    round_money(credited * rate, base_decimals).min(left_value_base)
                },
                amount: if last {
                    left_value
                } else {
                    round_money(amount * rate, base_decimals).min(left_value)
                },
            }
        });
        let direction = if entry.direction == "collected" {
            Direction::Collected
        } else {
            Direction::Paid
        };
        let account = match direction {
            Direction::Collected => entry.collected_account_id,
            Direction::Paid => entry.paid_account_id,
        }
        .ok_or_else(|| {
            Error::validation(
                "this company no longer says which accounts the invoice's tax code posts to",
            )
        })?;
        taxes.push(Tax {
            tax_code_id: entry.tax_code_id,
            direction,
            rate: entry.rate,
            base: -credited,
            amount: -amount,
            account_id: account,
            source_line_id,
            credits_id: Some(entry.id),
            group_code_id: entry.group_code_id,
            compound: entry.compound,
            foreign: foreign.map(|f| Converted {
                currency: f.currency,
                base: -f.base,
                amount: -f.amount,
            }),
            self_assessed: entry.self_assessed,
        });
    }
    Ok(taxes)
}

/// The tax to take back when an early-payment discount of `discount` is
/// taken on an open item of `gross` (both in the item's currency, more than
/// zero) that the posting `posting_id` opened: of each tax that posting
/// recorded, the same share of its base and its amount, at its code,
/// direction and rate, as negative amounts that credit the invoice's entry.
/// What has been taken back of an entry before (by a credit note) bounds
/// what can be. Empty when the invoice recorded no tax. Changes nothing;
/// record it with [`record`] under the payment's posting, so the tax return
/// shows it in the payment's period.
///
/// For an item in a foreign currency the tax is in that currency, and
/// `rate` (the payment's) gives what it is worth in the accounting
/// currency.
pub async fn discount(
    conn: &mut PgConnection,
    company_id: Uuid,
    posting_id: Uuid,
    discount: Decimal,
    gross: Decimal,
    rate: Decimal,
    source_line_id: Option<Uuid>,
) -> Result<Vec<Tax>> {
    #[derive(sqlx::FromRow)]
    struct Charged {
        id: Uuid,
        tax_code_id: Uuid,
        direction: String,
        rate: Decimal,
        group_code_id: Option<Uuid>,
        compound: bool,
        account_id: Option<Uuid>,
        currency: Option<String>,
        left_base: Decimal,
        left: Decimal,
        txn_base: Decimal,
        txn_amount: Decimal,
        txn_left_base: Decimal,
        txn_left: Decimal,
    }
    if gross <= Decimal::ZERO || discount <= Decimal::ZERO {
        return Ok(vec![]);
    }
    let charged: Vec<Charged> = sqlx::query_as(
        "SELECT e.id, e.tax_code_id, e.direction, e.rate, e.group_code_id, e.compound,
                COALESCE(e.account_id,
                         CASE e.direction WHEN 'collected' THEN s.collected_account_id
                                          ELSE s.paid_account_id END) AS account_id,
                e.currency,
                e.base + COALESCE((SELECT sum(c.base) FROM tax_entry c WHERE c.credits_id = e.id), 0)
                    AS left_base,
                e.amount + COALESCE((SELECT sum(c.amount) FROM tax_entry c
                                     WHERE c.credits_id = e.id), 0) AS left,
                COALESCE(e.txn_base, e.base) AS txn_base,
                COALESCE(e.txn_amount, e.amount) AS txn_amount,
                COALESCE(e.txn_base, e.base)
                    + COALESCE((SELECT sum(COALESCE(c.txn_base, c.base)) FROM tax_entry c
                                WHERE c.credits_id = e.id), 0) AS txn_left_base,
                COALESCE(e.txn_amount, e.amount)
                    + COALESCE((SELECT sum(COALESCE(c.txn_amount, c.amount)) FROM tax_entry c
                                WHERE c.credits_id = e.id), 0) AS txn_left
         FROM tax_entry e
         LEFT JOIN tax_settings s ON s.company_id = e.company_id AND s.tax_code_id = e.tax_code_id
         WHERE e.posting_id = $1 AND e.company_id = $2 AND e.reverses_id IS NULL
           AND e.credits_id IS NULL AND NOT e.self_assessed
           AND NOT EXISTS (SELECT 1 FROM tax_entry r WHERE r.reverses_id = e.id)
         ORDER BY e.line_no",
    )
    .bind(posting_id)
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    let base_decimals = company::currency_decimals(conn, company_id).await?;
    let mut taxes = Vec::with_capacity(charged.len());
    for entry in charged {
        let decimals = match &entry.currency {
            Some(currency) => {
                let found: Option<i16> =
                    sqlx::query_scalar("SELECT decimals FROM currency WHERE code = $1")
                        .bind(currency)
                        .fetch_optional(&mut *conn)
                        .await?;
                found.map_or(base_decimals, |d| d as u32)
            }
            None => base_decimals,
        };
        let share = |of: Decimal| round_money(of * discount / gross, decimals);
        let base = share(entry.txn_base).min(entry.txn_left_base);
        let amount = share(entry.txn_amount).min(entry.txn_left);
        if base <= Decimal::ZERO && amount <= Decimal::ZERO {
            continue;
        }
        let foreign = entry.currency.map(|currency| Converted {
            currency,
            base: -round_money(base * rate, base_decimals).min(entry.left_base),
            amount: -round_money(amount * rate, base_decimals).min(entry.left),
        });
        let account = entry.account_id.ok_or_else(|| {
            Error::validation(
                "this company no longer says which accounts the invoice's tax code posts to",
            )
        })?;
        taxes.push(Tax {
            tax_code_id: entry.tax_code_id,
            direction: if entry.direction == "collected" {
                Direction::Collected
            } else {
                Direction::Paid
            },
            rate: entry.rate,
            base: -base.max(Decimal::ZERO),
            amount: -amount.max(Decimal::ZERO),
            account_id: account,
            source_line_id,
            credits_id: Some(entry.id),
            group_code_id: entry.group_code_id,
            compound: entry.compound,
            foreign,
            self_assessed: false,
        });
    }
    Ok(taxes)
}

/// The tax a document's posting (that still stands) recorded, in the
/// accounting currency, as a positive amount: what it posted to the tax
/// accounts.
pub async fn posted_value(conn: &mut PgConnection, document_id: Uuid) -> Result<Decimal> {
    let value: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(e.amount), 0) FROM tax_entry e JOIN posting p ON p.id = e.posting_id
         WHERE p.document_id = $1 AND p.reverses_id IS NULL AND NOT e.self_assessed
           AND NOT EXISTS (SELECT 1 FROM posting r WHERE r.reverses_id = p.id)",
    )
    .bind(document_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(value.abs())
}

/// Record tax under `posting`, after the caller has posted it to the
/// general ledger. Can be called more than once for one posting.
pub async fn record(conn: &mut PgConnection, posting: &Posting, taxes: &[Tax]) -> Result<()> {
    let numbered: i32 =
        sqlx::query_scalar("SELECT COALESCE(max(line_no), 0) FROM tax_entry WHERE posting_id = $1")
            .bind(posting.id)
            .fetch_one(&mut *conn)
            .await?;
    for (index, tax) in taxes.iter().enumerate() {
        sqlx::query(
            "INSERT INTO tax_entry (id, company_id, posting_id, line_no, source_line_id,
                tax_code_id, direction, posting_date, rate, base, amount, credits_id,
                group_code_id, compound, currency, txn_base, txn_amount, account_id,
                self_assessed)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18, $19)",
        )
        .bind(new_id())
        .bind(posting.company_id)
        .bind(posting.id)
        .bind(numbered + index as i32 + 1)
        .bind(tax.source_line_id)
        .bind(tax.tax_code_id)
        .bind(tax.direction.as_str())
        .bind(posting.posting_date)
        .bind(tax.rate)
        // A return totals the accounting currency; the document's own
        // currency is kept beside it.
        .bind(tax.foreign.as_ref().map_or(tax.base, |f| f.base))
        .bind(tax.value())
        .bind(tax.credits_id)
        .bind(tax.group_code_id)
        .bind(tax.compound)
        .bind(tax.foreign.as_ref().map(|f| f.currency.as_str()))
        .bind(tax.foreign.as_ref().map(|_| tax.base))
        .bind(tax.foreign.as_ref().map(|_| tax.amount))
        .bind(tax.account_id)
        .bind(tax.self_assessed)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

struct TaxLedger;

/// One tax entry as a document shows it under its posting. Amounts are
/// decimals as text. The code's code and name are absent for a reader
/// who may not read tax codes.
#[derive(JsonSchema)]
#[allow(dead_code)] // describes the rows `entries` builds in SQL
struct TaxEntryRow {
    /// Entry id.
    id: Option<Uuid>,
    /// Position within the posting, from 1.
    line_no: Option<i32>,
    /// The document line the entry comes from.
    source_line_id: Option<Uuid>,
    tax_code_id: Option<Uuid>,
    /// Tax code.
    code: Option<String>,
    /// Name of the tax code.
    name: Option<String>,
    /// `collected` or `paid`.
    direction: Option<String>,
    /// Percent.
    rate: Option<String>,
    /// The net amount taxed.
    base: Option<String>,
    /// The tax.
    amount: Option<String>,
}

#[async_trait]
impl Ledger for TaxLedger {
    fn name(&self) -> &'static str {
        "tax"
    }

    fn label(&self) -> String {
        "Tax".into()
    }

    fn entry_schema(&self) -> serde_json::Value {
        erpcraft_kernel::document::row_schema::<TaxEntryRow>()
    }

    fn entry_tables(&self) -> &'static [&'static str] {
        &["tax_entry"]
    }

    /// Record the opposite of every entry of the original posting, dated
    /// as the reversal, so the tax leaves the period it is reversed in.
    async fn reverse(
        &self,
        conn: &mut PgConnection,
        original: &Posting,
        reversal: &Posting,
    ) -> Result<()> {
        // Each reversing entry gets a time-ordered id, in the order of the
        // original lines, so that it sorts after everything recorded
        // before it.
        let originals: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM tax_entry WHERE posting_id = $1 ORDER BY line_no")
                .bind(original.id)
                .fetch_all(&mut *conn)
                .await?;
        let ids: Vec<Uuid> = originals.iter().map(|_| new_id()).collect();
        // The trigger copies code, direction and rate from the original
        // and negates its amounts.
        sqlx::query(
            "INSERT INTO tax_entry (id, company_id, posting_id, line_no, source_line_id,
                tax_code_id, direction, posting_date, rate, base, amount, reverses_id)
             SELECT m.new_id, e.company_id, $2, e.line_no, e.source_line_id,
                e.tax_code_id, e.direction, $3, e.rate, 0, 0, e.id
             FROM unnest($1::uuid[], $4::uuid[]) AS m(old_id, new_id)
             JOIN tax_entry e ON e.id = m.old_id
             ORDER BY e.line_no",
        )
        .bind(&originals)
        .bind(reversal.id)
        .bind(reversal.posting_date)
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// The tax recorded under the posting, with its code. The code and
    /// name come only with the right to read tax codes.
    async fn entries(
        &self,
        ctx: &mut Ctx<'_>,
        posting: &Posting,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        let actor = ctx.actor;
        if !actor.can("tax_entry:read", posting.company_id) {
            return Ok(None);
        }
        let codes = actor.can_any("tax_code:read")
            && !["code", "name"]
                .iter()
                .any(|field| actor.may_withhold("tax_code:read", field));
        let rows = sqlx::query_scalar(
            "SELECT jsonb_build_object(
                'id', e.id, 'line_no', e.line_no, 'source_line_id', e.source_line_id,
                'tax_code_id', e.tax_code_id,
                'code', CASE WHEN $2 THEN c.code END,
                'name', CASE WHEN $2 THEN c.name END,
                'direction', e.direction, 'rate', e.rate::text,
                'base', e.base::text, 'amount', e.amount::text)
             FROM tax_entry e JOIN tax_code c ON c.id = e.tax_code_id
             WHERE e.posting_id = $1 ORDER BY e.line_no",
        )
        .bind(posting.id)
        .bind(codes)
        .fetch_all(&mut *ctx.conn)
        .await?;
        Ok(Some(rows))
    }
}

// ------------------------------------------------------------------- report

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReportInput {
    /// The company to report on.
    company_id: Uuid,
    /// First posting date to include.
    from: NaiveDate,
    /// Last posting date to include.
    to: NaiveDate,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
struct ReportRow {
    tax_code_id: Uuid,
    code: String,
    name: String,
    /// Net sales taxed with this code, and the tax charged on them.
    collected_base: Decimal,
    collected: Decimal,
    /// Net purchases taxed with this code, and the tax vendors charged.
    paid_base: Decimal,
    paid: Decimal,
}

#[derive(Serialize, JsonSchema)]
struct ReportOutput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    currency: String,
    rows: Vec<ReportRow>,
    total_collected: Decimal,
    total_paid: Decimal,
    /// Collected minus paid. Whether tax paid can be set off against tax
    /// collected depends on the country and the code.
    net: Decimal,
}

struct Report;

#[async_trait]
impl Command for Report {
    type Input = ReportInput;
    type Output = ReportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "tax.report",
            "Tax by code for a company over a range of posting dates: the net amount taxed and \
             the tax, separately for tax collected from customers and tax paid to vendors. \
             Reversals count in the period they were posted in. This is the raw material for a \
             tax return, not the return of any country.",
        )
        .report("Tax report")
        .permission("tax_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReportInput) -> Result<ReportOutput> {
        ctx.require("tax_entry:read", input.company_id)?;
        if input.from > input.to {
            return Err(Error::validation("'from' must not be after 'to'"));
        }
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let rows: Vec<ReportRow> = sqlx::query_as(
            "SELECT c.id AS tax_code_id, c.code, c.name,
                    COALESCE(sum(e.base) FILTER (WHERE e.direction = 'collected'), 0) AS collected_base,
                    COALESCE(sum(e.amount) FILTER (WHERE e.direction = 'collected'), 0) AS collected,
                    COALESCE(sum(e.base) FILTER (WHERE e.direction = 'paid'), 0) AS paid_base,
                    COALESCE(sum(e.amount) FILTER (WHERE e.direction = 'paid'), 0) AS paid
             FROM tax_entry e JOIN tax_code c ON c.id = e.tax_code_id
             WHERE e.company_id = $1 AND e.posting_date BETWEEN $2 AND $3
             GROUP BY c.id ORDER BY c.code",
        )
        .bind(input.company_id)
        .bind(input.from)
        .bind(input.to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let total_collected: Decimal = rows.iter().map(|r| r.collected).sum();
        let total_paid: Decimal = rows.iter().map(|r| r.paid).sum();
        Ok(ReportOutput {
            company_id: input.company_id,
            from: input.from,
            to: input.to,
            currency,
            rows,
            total_collected,
            total_paid,
            net: total_collected - total_paid,
        })
    }
}
