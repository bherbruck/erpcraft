//! Consolidation: the books of a group of companies added up in one
//! reporting currency, translated by the current rate method, with what the
//! members owe each other and sold each other taken out. A report only:
//! nothing is posted.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, EntityMeta, Error, FieldMeta, FieldType, JsonSchema,
    PgConnection, Result, async_trait, round_money, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) fn entities() -> Vec<EntityMeta> {
    let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
    vec![
        EntityMeta::new("consolidation_group", "Consolidation group")
            .field(FieldMeta::new("code", "Code", FieldType::Text).required())
            .field(FieldMeta::new("name", "Name", FieldType::Text).required())
            .field(
                FieldMeta::new(
                    "currency",
                    "Reporting currency",
                    FieldType::link_by("currency", "code"),
                )
                .required(),
            )
            .field(FieldMeta::new("active", "Active", FieldType::Bool))
            .field(created())
            .order_by(&["code"])
            .search(&["code", "name"]),
        EntityMeta::new("consolidation_member", "Consolidation member")
            .field(
                FieldMeta::new("group_id", "Group", FieldType::link("consolidation_group"))
                    .required()
                    .immutable(),
            )
            .field(
                FieldMeta::new("company_id", "Company", FieldType::link("company"))
                    .required()
                    .immutable()
                    .help("A company of the group, wholly owned."),
            )
            .field(created())
            .order_by(&["group_id", "company_id"])
            .display(&["company_id"])
            .child_of("consolidation_group", "group_id")
            .company_field("company_id"),
    ]
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RateInput {
    /// A member's accounting currency.
    currency: String,
    /// Reporting-currency units per unit of `currency` at `to`, for assets
    /// and liabilities.
    closing: Option<Decimal>,
    /// The same over the period, for income and expenses.
    average: Option<Decimal>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportInput {
    /// The consolidation group.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "consolidation_group", "key": "id" }))]
    group_id: Uuid,
    /// First day of the period of the income statement.
    from: NaiveDate,
    /// Last day of the period, and the day of the balance sheet.
    to: NaiveDate,
    /// Rates to use instead of those on record, per currency.
    #[serde(default)]
    rates: Vec<RateInput>,
}

#[derive(Serialize, JsonSchema, Clone, Default)]
pub(crate) struct Row {
    code: String,
    name: String,
    /// The members' balances translated into the reporting currency,
    /// positive in the section's natural direction.
    translated: Decimal,
    /// What eliminations take off (or put on), in the same direction.
    eliminated: Decimal,
    /// Translated plus eliminated.
    consolidated: Decimal,
}

#[derive(Serialize, JsonSchema, Default)]
pub(crate) struct Section {
    rows: Vec<Row>,
    total: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BalanceSheet {
    assets: Section,
    liabilities: Section,
    /// Equity accounts, then the currency translation adjustment and the
    /// intercompany difference as rows of their own.
    equity: Section,
    /// Income less expenses not closed to equity, translated.
    net_income: Decimal,
    /// What translation leaves: assets less liabilities beyond equity and
    /// results, all members.
    cta: Decimal,
    /// What the members' open balances with each other differ by.
    intercompany_difference: Decimal,
    total_assets: Decimal,
    /// Liabilities, equity and net income: equal to total_assets.
    total_liabilities_and_equity: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct IncomeStatement {
    income: Section,
    /// With a row "Intercompany cost eliminated".
    expenses: Section,
    net_income: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Member {
    company_id: Uuid,
    code: String,
    currency: String,
    closing_rate: Decimal,
    average_rate: Decimal,
    /// The member's own translation adjustment, in the reporting currency.
    cta: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Elimination {
    /// `balance`: open receivables against open payables at `to`.
    /// `trading`: the seller's revenue against the buyer's purchases in the
    /// period.
    kind: String,
    rule_id: Uuid,
    seller_code: String,
    buyer_code: String,
    /// The seller's side in the reporting currency.
    seller_amount: Decimal,
    /// The buyer's side in the reporting currency.
    buyer_amount: Decimal,
    /// Balances: both sides are taken out in full, each from its own
    /// account; this is the part they agree on. Trading: the seller's
    /// revenue, taken off income and off expenses alike.
    eliminated: Decimal,
    /// Seller's side less buyer's side.
    difference: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ReportOutput {
    group_id: Uuid,
    currency: String,
    from: NaiveDate,
    to: NaiveDate,
    members: Vec<Member>,
    balance_sheet: BalanceSheet,
    income_statement: IncomeStatement,
    eliminations: Vec<Elimination>,
}

pub(crate) struct Report;

/// One account of one member, debit less credit, in its currency, and
/// translated (all of it, for the balance sheet; the period's, for the
/// income statement).
struct Translated {
    code: String,
    name: String,
    account_type: String,
    balance: Decimal,
    period: Decimal,
}

/// The rate from `from` to `to` on each day from `first` to `last`, as
/// `erpcraft_gl::rate_on_record` reads it; the first day without one.
const DAILY: &str = "
    SELECT d::date AS day,
           (SELECT CASE WHEN r.from_currency = $1 THEN r.rate
                        WHEN r.rate = 0 THEN NULL
                        ELSE round(1 / r.rate, 18) END
            FROM exchange_rate r
            WHERE ((r.from_currency = $1 AND r.to_currency = $2)
                   OR (r.from_currency = $2 AND r.to_currency = $1))
              AND r.rate_date <= d::date
            ORDER BY r.rate_date DESC, (r.from_currency = $2) LIMIT 1) AS rate
    FROM generate_series($3::date, $4::date, interval '1 day') AS d";

async fn average_rate(
    conn: &mut PgConnection,
    from: &str,
    to: &str,
    first: NaiveDate,
    last: NaiveDate,
) -> Result<Decimal> {
    let days: Vec<(NaiveDate, Option<Decimal>)> = sqlx::query_as(DAILY)
        .bind(from)
        .bind(to)
        .bind(first)
        .bind(last)
        .fetch_all(&mut *conn)
        .await?;
    let mut sum = Decimal::ZERO;
    for (day, rate) in &days {
        sum += rate.ok_or_else(|| missing(from, to, *day))?;
    }
    Ok((sum / Decimal::from(days.len().max(1) as i64)).round_dp(18))
}

fn missing(from: &str, to: &str, day: NaiveDate) -> Error {
    Error::validation(format!(
        "no exchange rate from {from} to {to} is on record for {day}: record one, or state it \
         in `rates`"
    ))
}

/// An account's entries of one day: of the period's income statement,
/// and not.
type DayRow = (
    String,
    String,
    String,
    NaiveDate,
    Option<Decimal>,
    Option<Decimal>,
);

/// A member's accounts, translated.
async fn translate(
    conn: &mut PgConnection,
    company_id: Uuid,
    currency: &str,
    reporting: &str,
    (from, to): (NaiveDate, NaiveDate),
    (closing, average): (Decimal, Decimal),
    decimals: u32,
) -> Result<Vec<Translated>> {
    // Per account and day: what is of the period's income statement, and
    // what is not; the historical rate of each day.
    let rows: Vec<DayRow> = sqlx::query_as(
        "SELECT a.code, a.name, a.account_type, g.posting_date,
                sum(g.debit - g.credit) FILTER (WHERE g.posting_date >= $2
                    AND NOT starts_with(p.command, 'year_end_close.')) AS period,
                sum(g.debit - g.credit) FILTER (WHERE g.posting_date < $2
                    OR starts_with(p.command, 'year_end_close.')) AS other
         FROM gl_entry g JOIN account a ON a.id = g.account_id
         JOIN posting p ON p.id = g.posting_id
         WHERE g.company_id = $1 AND g.posting_date <= $3
         GROUP BY a.code, a.name, a.account_type, g.posting_date",
    )
    .bind(company_id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await?;
    let mut historical: BTreeMap<NaiveDate, Decimal> = BTreeMap::new();
    let mut out: BTreeMap<String, Translated> = BTreeMap::new();
    for (code, name, account_type, day, period, other) in rows {
        let (period, other) = (period.unwrap_or_default(), other.unwrap_or_default());
        let entry = out.entry(code.clone()).or_insert_with(|| Translated {
            code,
            name,
            account_type: account_type.clone(),
            balance: Decimal::ZERO,
            period: Decimal::ZERO,
        });
        match account_type.as_str() {
            "asset" | "liability" => entry.balance += (period + other) * closing,
            _ => {
                // Equity at the historical rate, all of it; results of
                // the period at the average, the rest historically.
                let (in_period, hist) = if account_type == "equity" {
                    (Decimal::ZERO, period + other)
                } else {
                    (period, other)
                };
                let rate = if hist.is_zero() || currency == reporting {
                    Decimal::ONE
                } else if let Some(rate) = historical.get(&day) {
                    *rate
                } else {
                    let rate = erpcraft_gl::rate_on_record(&mut *conn, currency, reporting, day)
                        .await?
                        .ok_or_else(|| missing(currency, reporting, day))?;
                    historical.insert(day, rate);
                    rate
                };
                entry.period += in_period * average;
                entry.balance += in_period * average + hist * rate;
            }
        }
    }
    Ok(out
        .into_values()
        .map(|mut t| {
            t.balance = round_money(t.balance, decimals);
            t.period = round_money(t.period, decimals);
            t
        })
        .collect())
}

/// Debit less credit of the open items of one party in a company as of a
/// day, at their carried value, per account code.
async fn open_by_account(
    conn: &mut PgConnection,
    company_id: Uuid,
    kind: &str,
    (party_type, party_id): (&str, Uuid),
    day: NaiveDate,
) -> Result<Vec<(String, Decimal)>> {
    let rows: Vec<(String, Option<Decimal>)> = sqlx::query_as(
        "SELECT a.code, sum(i.amount - COALESCE((
                    SELECT sum(s.amount) FROM settlement s JOIN posting sp ON sp.id = s.posting_id
                    WHERE s.open_item_id = i.id AND sp.posting_date <= $5), 0)
                + COALESCE((
                    SELECT sum(v.amount) FROM open_item_revaluation v
                    JOIN posting vp ON vp.id = v.posting_id
                    WHERE v.open_item_id = i.id AND vp.posting_date <= $5), 0))
         FROM open_item i JOIN posting p ON p.id = i.posting_id
         JOIN account a ON a.id = i.account_id
         WHERE i.company_id = $1 AND i.kind = $2 AND i.party_type = $3 AND i.party_id = $4
           AND p.posting_date <= $5
         GROUP BY a.code",
    )
    .bind(company_id)
    .bind(kind)
    .bind(party_type)
    .bind(party_id)
    .bind(day)
    .fetch_all(&mut *conn)
    .await?;
    // A receivable is a debit balance, a payable a credit one.
    let sign = if kind == "receivable" {
        Decimal::ONE
    } else {
        -Decimal::ONE
    };
    Ok(rows
        .into_iter()
        .map(|(code, open)| (code, open.unwrap_or_default() * sign))
        .collect())
}

#[async_trait]
impl Command for Report {
    type Input = ReportInput;
    type Output = ReportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "consolidation.report",
            "Consolidated financial statements of a group of companies of this database, in the \
             group's reporting currency: each member's accounts, added up by account code, \
             translated by the current rate method (assets and liabilities at the closing rate \
             of `to`, income and expenses of the period at the average rate of its days, \
             equity and older results at the rate of each entry's day), with the currency \
             translation adjustment in equity; intercompany open balances at `to` and \
             intercompany revenue of the period eliminated per intercompany rule between \
             members, each with its two sides and their difference. A report only: nothing \
             is posted. `rates` overrides the closing and average rate of a currency. Takes \
             the right to read ledger entries in every member.",
        )
        .report("Consolidated financial statements")
        .permission("gl_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReportInput) -> Result<ReportOutput> {
        if input.from > input.to {
            return Err(Error::validation("`from` cannot be after `to`"));
        }
        let group: Option<(String,)> =
            sqlx::query_as("SELECT currency FROM consolidation_group WHERE id = $1")
                .bind(input.group_id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
        let (reporting,) = group.ok_or_else(|| Error::not_found("consolidation_group"))?;
        let members: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT c.id, c.code, c.base_currency FROM consolidation_member m
             JOIN company c ON c.id = m.company_id WHERE m.group_id = $1 ORDER BY c.code",
        )
        .bind(input.group_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        if members.is_empty() {
            return Err(Error::validation("the group has no member companies"));
        }
        for (company_id, _, _) in &members {
            ctx.require("gl_entry:read", *company_id)?;
        }
        let decimals = erpcraft_gl::currency_decimals(&mut *ctx.conn, &reporting).await?;
        let (from, to) = (input.from, input.to);

        // Rates and translated accounts, per member.
        let mut rates: BTreeMap<Uuid, (Decimal, Decimal)> = BTreeMap::new();
        let mut members_out = vec![];
        let mut accounts: BTreeMap<String, (String, String, Decimal, Decimal)> = BTreeMap::new();
        let mut cta_total = Decimal::ZERO;
        for (company_id, code, currency) in &members {
            let stated = input.rates.iter().find(|r| &r.currency == currency);
            let (closing, average) = if currency == &reporting {
                (Decimal::ONE, Decimal::ONE)
            } else {
                let closing = match stated.and_then(|r| r.closing) {
                    Some(rate) => rate,
                    None => erpcraft_gl::rate_on_record(&mut *ctx.conn, currency, &reporting, to)
                        .await?
                        .ok_or_else(|| missing(currency, &reporting, to))?,
                };
                let average = match stated.and_then(|r| r.average) {
                    Some(rate) => rate,
                    None => average_rate(&mut *ctx.conn, currency, &reporting, from, to).await?,
                };
                (closing, average)
            };
            rates.insert(*company_id, (closing, average));
            let translated = translate(
                &mut *ctx.conn,
                *company_id,
                currency,
                &reporting,
                (from, to),
                (closing, average),
                decimals,
            )
            .await?;
            // What translation leaves of this member: the plug that makes
            // it balance again, in equity.
            let leftover: Decimal = translated.iter().map(|t| t.balance).sum();
            let cta = leftover;
            cta_total += cta;
            for t in translated {
                let entry = accounts.entry(t.code.clone()).or_insert((
                    t.name.clone(),
                    t.account_type.clone(),
                    Decimal::ZERO,
                    Decimal::ZERO,
                ));
                entry.2 += t.balance;
                entry.3 += t.period;
            }
            members_out.push(Member {
                company_id: *company_id,
                code: code.clone(),
                currency: currency.clone(),
                closing_rate: closing,
                average_rate: average,
                cta,
            });
        }

        // Eliminations, per rule between two members, signed debit less
        // credit per account code: the balance sheet's and the period's.
        let mut eliminated: BTreeMap<String, Decimal> = BTreeMap::new();
        let mut eliminated_period: BTreeMap<String, Decimal> = BTreeMap::new();
        let mut eliminations = vec![];
        let mut difference_total = Decimal::ZERO;
        let mut cost_eliminated = Decimal::ZERO;
        let ids: Vec<Uuid> = members.iter().map(|m| m.0).collect();
        let rules: Vec<(Uuid, Uuid, Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, company_id, buyer_company_id, customer_id, vendor_id
             FROM intercompany_rule
             WHERE company_id = ANY ($1) AND buyer_company_id = ANY ($1)
             ORDER BY created_at, id",
        )
        .bind(&ids)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let code_of = |id: Uuid| {
            members
                .iter()
                .find(|m| m.0 == id)
                .map(|m| m.1.clone())
                .unwrap_or_default()
        };
        for (rule, seller, buyer, customer, vendor) in rules {
            let (seller_closing, seller_average) = rates[&seller];
            let (buyer_closing, buyer_average) = rates[&buyer];
            // Balances at `to`.
            let receivable = open_by_account(
                &mut *ctx.conn,
                seller,
                "receivable",
                ("customer", customer),
                to,
            )
            .await?;
            let payable =
                open_by_account(&mut *ctx.conn, buyer, "payable", ("vendor", vendor), to).await?;
            let mut seller_amount = Decimal::ZERO;
            let mut buyer_amount = Decimal::ZERO;
            for (code, open) in receivable {
                let value = round_money(open * seller_closing, decimals);
                seller_amount += value;
                *eliminated.entry(code).or_default() -= value;
            }
            for (code, open) in payable {
                let value = round_money(open * buyer_closing, decimals);
                buyer_amount -= value;
                *eliminated.entry(code).or_default() -= value;
            }
            if !seller_amount.is_zero() || !buyer_amount.is_zero() {
                difference_total += seller_amount - buyer_amount;
                eliminations.push(Elimination {
                    kind: "balance".into(),
                    rule_id: rule,
                    seller_code: code_of(seller),
                    buyer_code: code_of(buyer),
                    seller_amount,
                    buyer_amount,
                    eliminated: seller_amount.min(buyer_amount).max(Decimal::ZERO),
                    difference: seller_amount - buyer_amount,
                });
            }

            // Trading in the period: the seller's revenue from documents
            // of this customer, and the buyer's net of their mirrors.
            let revenue: Vec<(String, Option<Decimal>)> = sqlx::query_as(
                "WITH docs AS (
                     SELECT DISTINCT p.document_id FROM open_item i
                     JOIN posting p ON p.id = i.posting_id
                     JOIN document d ON d.id = p.document_id
                     WHERE i.company_id = $1 AND i.party_type = 'customer' AND i.party_id = $2
                       AND d.doc_type IN ('sales_invoice', 'sales_credit_note', 'sales_credit'))
                 SELECT a.code, sum(g.credit - g.debit)
                 FROM gl_entry g JOIN posting p ON p.id = g.posting_id
                 JOIN account a ON a.id = g.account_id
                 WHERE p.document_id IN (SELECT document_id FROM docs)
                   AND a.account_type = 'income'
                   AND g.posting_date BETWEEN $3 AND $4
                 GROUP BY a.code",
            )
            .bind(seller)
            .bind(customer)
            .bind(from)
            .bind(to)
            .fetch_all(&mut *ctx.conn)
            .await?;
            let mut sold = Decimal::ZERO;
            for (code, amount) in revenue {
                let value = round_money(amount.unwrap_or_default() * seller_average, decimals);
                sold += value;
                // Revenue is a credit: taking it off is a debit.
                *eliminated.entry(code.clone()).or_default() += value;
                *eliminated_period.entry(code).or_default() += value;
            }
            let bought: Option<Decimal> = sqlx::query_scalar(
                "SELECT sum((m.summary->>'total_net_accounting')::numeric)
                 FROM intercompany_document x JOIN document s ON s.id = x.source_id
                 JOIN document m ON m.id = x.mirror_id
                 JOIN posting p ON p.document_id = m.id AND p.reverses_id IS NULL
                 WHERE x.rule_id = $1 AND s.doc_type = 'sales_invoice' AND m.status = 'posted'
                   AND p.posting_date BETWEEN $2 AND $3",
            )
            .bind(rule)
            .bind(from)
            .bind(to)
            .fetch_one(&mut *ctx.conn)
            .await?;
            let bought = round_money(bought.unwrap_or_default() * buyer_average, decimals);
            if !sold.is_zero() || !bought.is_zero() {
                cost_eliminated += sold;
                eliminations.push(Elimination {
                    kind: "trading".into(),
                    rule_id: rule,
                    seller_code: code_of(seller),
                    buyer_code: code_of(buyer),
                    seller_amount: sold,
                    buyer_amount: bought,
                    eliminated: sold,
                    difference: sold - bought,
                });
            }
        }

        // The sections. Signed debit less credit throughout; a section
        // shows its natural direction.
        let mut assets = Section::default();
        let mut liabilities = Section::default();
        let mut equity = Section::default();
        let mut income = Section::default();
        let mut expenses = Section::default();
        let mut results = Decimal::ZERO;
        for (code, (name, account_type, balance, period)) in &accounts {
            let off = eliminated.get(code).copied().unwrap_or_default();
            let off_period = eliminated_period.get(code).copied().unwrap_or_default();
            let row = |amount: Decimal, off: Decimal, debit_side: bool| {
                let sign = if debit_side {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                Row {
                    code: code.clone(),
                    name: name.clone(),
                    translated: amount * sign,
                    eliminated: off * sign,
                    consolidated: (amount + off) * sign,
                }
            };
            match account_type.as_str() {
                "asset" => assets.rows.push(row(*balance, off, true)),
                "liability" => liabilities.rows.push(row(*balance, off, false)),
                "equity" => equity.rows.push(row(*balance, off, false)),
                kind => {
                    results += balance + off;
                    if !period.is_zero() || !off_period.is_zero() {
                        if kind == "income" {
                            income.rows.push(row(*period, off_period, false));
                        } else {
                            expenses.rows.push(row(*period, off_period, true));
                        }
                    }
                }
            }
        }
        // Eliminations on accounts no member has a balance on are kept too.
        for (code, off) in &eliminated {
            if !accounts.contains_key(code) && !off.is_zero() {
                assets.rows.push(Row {
                    code: code.clone(),
                    name: code.clone(),
                    translated: Decimal::ZERO,
                    eliminated: *off,
                    consolidated: *off,
                });
            }
        }
        if !cost_eliminated.is_zero() {
            expenses.rows.push(Row {
                code: String::new(),
                name: "Intercompany cost eliminated".into(),
                translated: Decimal::ZERO,
                eliminated: -cost_eliminated,
                consolidated: -cost_eliminated,
            });
            // The same off the balance sheet's results: they do not change.
            results -= cost_eliminated;
        }
        // Equity: the translation adjustment and the intercompany
        // difference keep the balance sheet balanced.
        equity.rows.push(Row {
            code: String::new(),
            name: "Currency translation adjustment".into(),
            translated: cta_total,
            eliminated: Decimal::ZERO,
            consolidated: cta_total,
        });
        if !difference_total.is_zero() {
            equity.rows.push(Row {
                code: String::new(),
                name: "Intercompany difference".into(),
                translated: Decimal::ZERO,
                eliminated: -difference_total,
                consolidated: -difference_total,
            });
        }
        for section in [
            &mut assets,
            &mut liabilities,
            &mut equity,
            &mut income,
            &mut expenses,
        ] {
            section.total = section.rows.iter().map(|r| r.consolidated).sum();
        }
        let net_income = -results;
        let period_income = income.total - expenses.total;
        let total_liabilities_and_equity = liabilities.total + equity.total + net_income;
        Ok(ReportOutput {
            group_id: input.group_id,
            currency: reporting.clone(),
            from,
            to,
            members: members_out,
            balance_sheet: BalanceSheet {
                total_assets: assets.total,
                total_liabilities_and_equity,
                net_income,
                cta: cta_total,
                intercompany_difference: difference_total,
                assets,
                liabilities,
                equity,
            },
            income_statement: IncomeStatement {
                income,
                expenses,
                net_income: period_income,
            },
            eliminations,
        })
    }
}
