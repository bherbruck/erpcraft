//! Budgets: what a company plans, by account, month and optionally a
//! dimension value, imported from CSV or copied from last year's actuals,
//! and reported against the general ledger. See "Budgets" in
//! ARCHITECTURE.md.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Datelike, Months, NaiveDate};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, DynCommand, EntityMeta, Error, FieldMeta, FieldType,
    JsonSchema, Migration, Module, PgConnection, Result, async_trait, command, company, new_id,
    round_money, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub struct BudgetsModule;

impl Module for BudgetsModule {
    fn name(&self) -> &'static str {
        "budgets"
    }

    fn label(&self) -> String {
        "Budgets".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "budgets",
                sql: include_str!("../migrations/0001_budgets.sql"),
            },
            Migration {
                version: 2,
                name: "line_company",
                sql: include_str!("../migrations/0002_line_company.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("budget", "Budget")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new("starts_on", "Starts on", FieldType::Date)
                        .required()
                        .help("The first day of a month, usually of the fiscal year."),
                )
                .field(
                    FieldMeta::new("ends_on", "Ends on", FieldType::Date)
                        .required()
                        .help("The last day of a month; at most 24 months after the start."),
                )
                .field(FieldMeta::new("version", "Version", FieldType::Text).help(
                    "original (the default), forecast, or another name: one of each per \
                         start.",
                ))
                .field(
                    FieldMeta::new("dimension_id", "By dimension", FieldType::link("dimension"))
                        .help("Lines may name a value of it, such as a site or a cost center."),
                )
                .field(
                    FieldMeta::new("locked", "Locked", FieldType::Bool)
                        .help("No change to the budget or its lines until it is unlocked."),
                )
                .field(created())
                .order_by(&["starts_on", "code"])
                .search(&["code", "name"])
                .company_field("company_id"),
            EntityMeta::new("budget_line", "Budget line")
                .field(
                    FieldMeta::new("budget_id", "Budget", FieldType::link("budget"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("account_id", "Account", FieldType::link("account")).required(),
                )
                .field(
                    FieldMeta::new("month", "Month", FieldType::Date)
                        .required()
                        .help("The first day of the month."),
                )
                .field(FieldMeta::new(
                    "dimension_value_id",
                    "Dimension value",
                    FieldType::link("dimension_value"),
                ))
                .field(
                    FieldMeta::new("amount", "Amount", FieldType::Decimal)
                        .money()
                        .required()
                        .help(
                            "In the account's natural direction: a cost as a positive expense, \
                             a sale as positive income.",
                        ),
                )
                .field(created())
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .immutable()
                        .help("The budget's company; filled in from the budget."),
                )
                .order_by(&["month", "account_id"])
                .child_of("budget", "budget_id")
                .company_field("company_id"),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Import), command(CopyActuals), command(Report)]
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Budget {
    id: Uuid,
    company_id: Uuid,
    code: String,
    starts_on: NaiveDate,
    ends_on: NaiveDate,
    dimension_id: Option<Uuid>,
    locked: bool,
}

async fn budget(ctx: &mut Ctx<'_>, id: Uuid, permission: &str) -> Result<Budget> {
    let b: Budget = sqlx::query_as(
        "SELECT id, company_id, code, starts_on, ends_on, dimension_id, locked
         FROM budget WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *ctx.conn)
    .await?
    .ok_or_else(|| Error::not_found("budget"))?;
    ctx.require(permission, b.company_id)?;
    if permission != "budget:read" && b.locked {
        return Err(Error::state(format!("budget {} is locked", b.code)));
    }
    Ok(b)
}

/// Debit minus credit per account, month and value of `dimension` over a
/// range, closing entries left out, turned to the accounts' natural
/// direction.
async fn actuals(
    conn: &mut PgConnection,
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    dimension: Option<Uuid>,
    types: &[String],
) -> Result<Vec<(Uuid, NaiveDate, Option<Uuid>, Decimal)>> {
    Ok(sqlx::query_as(
        "SELECT g.account_id, date_trunc('month', g.posting_date)::date, v.value_id,
                CASE WHEN a.account_type IN ('asset', 'expense')
                     THEN sum(g.debit) - sum(g.credit) ELSE sum(g.credit) - sum(g.debit) END
         FROM gl_entry g
         JOIN account a ON a.id = g.account_id
         JOIN posting p ON p.id = g.posting_id
         LEFT JOIN dimension_set_value v
             ON v.set_id = g.dimension_set_id AND v.dimension_id = $4
         WHERE g.company_id = $1 AND g.posting_date BETWEEN $2 AND $3
           AND NOT starts_with(p.command, 'year_end_close.')
           AND a.account_type = ANY ($5)
         GROUP BY g.account_id, a.account_type, 2, v.value_id
         HAVING sum(g.debit) <> sum(g.credit)",
    )
    .bind(company_id)
    .bind(from)
    .bind(to)
    .bind(dimension)
    .bind(types)
    .fetch_all(&mut *conn)
    .await?)
}

/// Write a budget line, replacing one of the same account, month and value.
async fn put_line(
    conn: &mut PgConnection,
    budget: Uuid,
    account: Uuid,
    month: NaiveDate,
    value: Option<Uuid>,
    amount: Decimal,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO budget_line (id, budget_id, account_id, month, dimension_value_id, amount)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT ON CONSTRAINT budget_line_once DO UPDATE SET amount = EXCLUDED.amount",
    )
    .bind(new_id())
    .bind(budget)
    .bind(account)
    .bind(month)
    .bind(value)
    .bind(amount)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ----------------------------------------------------------------- import

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportInput {
    /// The budget to fill.
    id: Uuid,
    /// CSV with a header. Long: `account`, `month` (YYYY-MM), `amount` and
    /// optionally `dimension_value`. Wide: `account`, optionally
    /// `dimension_value`, and a column per month headed YYYY-MM. Accounts
    /// and values by their codes.
    csv: String,
    /// Remove the budget's lines first. Otherwise a line replaces the one
    /// of the same account, month and value.
    #[serde(default)]
    replace: bool,
}

#[derive(Serialize, JsonSchema)]
struct ImportOutput {
    /// Lines written.
    lines: u32,
}

struct Import;

fn month_of(text: &str) -> Option<NaiveDate> {
    let (y, m) = text.trim().split_once('-')?;
    if y.len() != 4 || m.len() != 2 {
        return None;
    }
    NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, 1)
}

#[async_trait]
impl Command for Import {
    type Input = ImportInput;
    type Output = ImportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "budget.import",
            "Fill a budget from CSV, long (account, month as YYYY-MM, amount, optional \
             dimension_value) or wide (account, optional dimension_value, a column per month \
             headed YYYY-MM). Accounts and dimension values by code; amounts in each account's \
             natural direction. A line replaces the one of the same account, month and value; \
             replace clears the budget first. All or nothing.",
        )
        .permission("budget:update")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ImportInput) -> Result<ImportOutput> {
        let b = budget(ctx, input.id, "budget:update").await?;
        let decimals = company::currency_decimals(&mut *ctx.conn, b.company_id).await?;
        let mut reader = csv::ReaderBuilder::new()
            .trim(csv::Trim::All)
            .from_reader(input.csv.as_bytes());
        let headers: Vec<String> = reader
            .headers()
            .map_err(|e| Error::validation(format!("the header: {e}")).with_field("csv"))?
            .iter()
            .map(|h| h.to_lowercase())
            .collect();
        let col = |name: &str| headers.iter().position(|h| h == name);
        let account_col = col("account")
            .ok_or_else(|| Error::validation("the CSV has no account column").with_field("csv"))?;
        let value_col = col("dimension_value");
        let long = col("month").zip(col("amount"));
        let month_cols: Vec<(usize, NaiveDate)> = headers
            .iter()
            .enumerate()
            .filter_map(|(i, h)| month_of(h).map(|m| (i, m)))
            .collect();
        if long.is_none() && month_cols.is_empty() {
            return Err(Error::validation(
                "the CSV has neither month and amount columns nor a column per month",
            )
            .with_field("csv"));
        }
        if input.replace {
            sqlx::query("DELETE FROM budget_line WHERE budget_id = $1")
                .bind(b.id)
                .execute(&mut *ctx.conn)
                .await?;
        }
        let mut accounts: BTreeMap<String, Uuid> = BTreeMap::new();
        let mut values: BTreeMap<String, Uuid> = BTreeMap::new();
        let mut written = 0;
        for (index, record) in reader.records().enumerate() {
            let line = index + 2;
            let bad =
                |why: String| Error::validation(format!("line {line}: {why}")).with_field("csv");
            let record = record.map_err(|e| bad(e.to_string()))?;
            let get = |i: usize| record.get(i).unwrap_or("").trim();
            let code = get(account_col);
            let account = match accounts.get(code) {
                Some(a) => *a,
                None => {
                    let found: Option<(Uuid, bool)> = sqlx::query_as(
                        "SELECT id, is_group FROM account WHERE company_id = $1 AND code = $2",
                    )
                    .bind(b.company_id)
                    .bind(code)
                    .fetch_optional(&mut *ctx.conn)
                    .await?;
                    match found {
                        Some((id, false)) => {
                            accounts.insert(code.to_owned(), id);
                            id
                        }
                        Some((_, true)) => {
                            return Err(bad(format!("account {code} is a group account")));
                        }
                        None => return Err(bad(format!("there is no account {code}"))),
                    }
                }
            };
            let value = match value_col.map(get).filter(|v| !v.is_empty()) {
                None => None,
                Some(v) => {
                    let dimension = b
                        .dimension_id
                        .ok_or_else(|| bad("the budget is not kept by a dimension".into()))?;
                    match values.get(v) {
                        Some(id) => Some(*id),
                        None => {
                            let id: Uuid = sqlx::query_scalar(
                                "SELECT id FROM dimension_value WHERE dimension_id = $1 AND code = $2",
                            )
                            .bind(dimension)
                            .bind(v)
                            .fetch_optional(&mut *ctx.conn)
                            .await?
                            .ok_or_else(|| {
                                bad(format!("the budget's dimension has no value {v}"))
                            })?;
                            values.insert(v.to_owned(), id);
                            Some(id)
                        }
                    }
                }
            };
            let mut cells: Vec<(NaiveDate, &str)> = vec![];
            if let Some((m, a)) = long {
                let month = month_of(get(m))
                    .ok_or_else(|| bad(format!("{} is not a month as YYYY-MM", get(m))))?;
                cells.push((month, get(a)));
            }
            for (i, month) in &month_cols {
                if !get(*i).is_empty() {
                    cells.push((*month, get(*i)));
                }
            }
            for (month, text) in cells {
                if month < b.starts_on || month > b.ends_on {
                    return Err(bad(format!(
                        "{} is outside the budget",
                        month.format("%Y-%m")
                    )));
                }
                let amount: Decimal = text
                    .parse()
                    .map_err(|_| bad(format!("{text} is not an amount")))?;
                if amount.normalize().scale() > decimals {
                    return Err(bad(format!(
                        "{text} has more than {decimals} decimal places"
                    )));
                }
                put_line(&mut *ctx.conn, b.id, account, month, value, amount).await?;
                written += 1;
            }
        }
        ctx.touch("budget", b.id);
        Ok(ImportOutput { lines: written })
    }
}

// ------------------------------------------------------------ copy actuals

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CopyInput {
    /// The budget to fill.
    id: Uuid,
    /// Percent to change the actuals by: 5 for five percent more, -10 for
    /// ten percent less. 0 when omitted.
    #[serde(default)]
    percent: Decimal,
    /// The account types to copy: income and expense when omitted.
    account_types: Option<Vec<String>>,
    /// The first month to copy from; a year before the budget's start when
    /// omitted.
    from: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema)]
struct CopyOutput {
    lines: u32,
}

struct CopyActuals;

#[async_trait]
impl Command for CopyActuals {
    type Input = CopyInput;
    type Output = CopyOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "budget.copy_actuals",
            "Fill a budget with what the general ledger posted in the same months a year \
             earlier (or from `from`), per account and per value of the budget's dimension, \
             changed by `percent` and rounded to the currency's cents. Replaces the budget's \
             lines of those account types (income and expense by default). Year-end closing \
             entries are left out.",
        )
        .permission("budget:update")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CopyInput) -> Result<CopyOutput> {
        let b = budget(ctx, input.id, "budget:update").await?;
        ctx.require("gl_entry:read", b.company_id)?;
        let types = input
            .account_types
            .unwrap_or_else(|| vec!["income".into(), "expense".into()]);
        if let Some(t) = types
            .iter()
            .find(|t| !["asset", "liability", "equity", "income", "expense"].contains(&t.as_str()))
        {
            return Err(Error::validation(format!("{t} is not an account type"))
                .with_field("account_types"));
        }
        let from = input
            .from
            .map(|d| d.with_day(1).unwrap_or(d))
            .or_else(|| b.starts_on.checked_sub_months(Months::new(12)))
            .ok_or_else(|| Error::validation("no such month").with_field("from"))?;
        let shift = (b.starts_on.year() - from.year()) * 12 + b.starts_on.month() as i32
            - from.month() as i32;
        let span = (b.ends_on.year() - b.starts_on.year()) * 12 + b.ends_on.month() as i32
            - b.starts_on.month() as i32;
        let to = from
            .checked_add_months(Months::new(span as u32 + 1))
            .and_then(|d| d.pred_opt())
            .ok_or_else(|| Error::validation("no such month").with_field("from"))?;
        let decimals = company::currency_decimals(&mut *ctx.conn, b.company_id).await?;
        sqlx::query(
            "DELETE FROM budget_line l USING account a
             WHERE l.budget_id = $1 AND a.id = l.account_id AND a.account_type = ANY ($2)",
        )
        .bind(b.id)
        .bind(&types)
        .execute(&mut *ctx.conn)
        .await?;
        let factor = (Decimal::ONE_HUNDRED + input.percent) / Decimal::ONE_HUNDRED;
        let mut written = 0;
        for (account, month, value, amount) in actuals(
            &mut *ctx.conn,
            b.company_id,
            from,
            to,
            b.dimension_id,
            &types,
        )
        .await?
        {
            let target = if shift >= 0 {
                month.checked_add_months(Months::new(shift as u32))
            } else {
                month.checked_sub_months(Months::new((-shift) as u32))
            };
            let Some(target) = target else { continue };
            put_line(
                &mut *ctx.conn,
                b.id,
                account,
                target,
                value,
                round_money(amount * factor, decimals),
            )
            .await?;
            written += 1;
        }
        ctx.touch("budget", b.id);
        Ok(CopyOutput { lines: written })
    }
}

// ----------------------------------------------------------------- report

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReportInput {
    /// The budget to report on.
    id: Uuid,
    /// The first month to report; the budget's start when omitted.
    from: Option<NaiveDate>,
    /// The last day to report; the budget's end when omitted.
    to: Option<NaiveDate>,
    /// A row per month rather than one for the range.
    #[serde(default)]
    by_month: bool,
    /// Only this value of the budget's dimension.
    dimension_value_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema, Clone)]
struct ReportRow {
    account_id: Uuid,
    code: String,
    name: String,
    account_type: String,
    /// With `by_month`: the first day of the month.
    month: Option<NaiveDate>,
    dimension_value_id: Option<Uuid>,
    dimension_value_code: Option<String>,
    budget: Decimal,
    actual: Decimal,
    /// Actual minus budget, in the account's natural direction.
    variance: Decimal,
    /// The variance as a percent of the budget; none when the budget is 0.
    variance_percent: Option<Decimal>,
    /// More income or less cost than planned.
    favourable: bool,
}

#[derive(Serialize, JsonSchema, Default)]
struct Totals {
    budget: Decimal,
    actual: Decimal,
    variance: Decimal,
}

#[derive(Serialize, JsonSchema)]
struct PeriodTotals {
    /// With `by_month`: the month; else none, for the range.
    month: Option<NaiveDate>,
    income: Totals,
    expense: Totals,
    /// Income minus expense.
    result: Totals,
}

#[derive(Serialize, JsonSchema)]
struct ReportOutput {
    budget_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    currency: String,
    rows: Vec<ReportRow>,
    totals: Vec<PeriodTotals>,
}

struct Report;

#[async_trait]
impl Command for Report {
    type Input = ReportInput;
    type Output = ReportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "budget.report",
            "Budget against actual: per account (and value of the budget's dimension, and month \
             with by_month), the budget, what the general ledger posted (closing entries left \
             out), the variance (actual minus budget, in the account's natural direction), the \
             variance in percent of the budget and whether it is favourable; with income, \
             expense and result totals per month and for the range.",
        )
        .report("Budget against actual")
        .permission("budget:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ReportInput) -> Result<ReportOutput> {
        let b = budget(ctx, input.id, "budget:read").await?;
        ctx.require("gl_entry:read", b.company_id)?;
        let from = input
            .from
            .map(|d| d.with_day(1).unwrap_or(d))
            .unwrap_or(b.starts_on)
            .max(b.starts_on);
        let to = input.to.unwrap_or(b.ends_on).min(b.ends_on);
        if from > to {
            return Err(Error::validation("`from` cannot be after `to`"));
        }
        let currency = company::base_currency(&mut *ctx.conn, b.company_id).await?;
        let budgeted: Vec<(Uuid, NaiveDate, Option<Uuid>, Decimal)> = sqlx::query_as(
            "SELECT account_id, month, dimension_value_id, amount FROM budget_line
             WHERE budget_id = $1 AND month BETWEEN $2 AND $3",
        )
        .bind(b.id)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let all_types: Vec<String> = ["asset", "liability", "equity", "income", "expense"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        let posted = actuals(
            &mut *ctx.conn,
            b.company_id,
            from,
            to,
            b.dimension_id,
            &all_types,
        )
        .await?;
        // Accounts that were budgeted, and what they posted.
        let budgeted_accounts: Vec<Uuid> = budgeted.iter().map(|l| l.0).collect();
        type Key = (Uuid, Option<NaiveDate>, Option<Uuid>);
        let mut cells: BTreeMap<Key, (Decimal, Decimal)> = BTreeMap::new();
        let keep = |value: Option<Uuid>| {
            input.dimension_value_id.is_none() || value == input.dimension_value_id
        };
        for (account, month, value, amount) in &budgeted {
            if keep(*value) {
                let key = (*account, input.by_month.then_some(*month), *value);
                cells.entry(key).or_default().0 += *amount;
            }
        }
        for (account, month, value, amount) in &posted {
            // Income and expense accounts always; others when budgeted.
            if keep(*value) {
                let key = (*account, input.by_month.then_some(*month), *value);
                cells.entry(key).or_default().1 += *amount;
            }
        }
        let accounts: Vec<(Uuid, String, String, String)> = sqlx::query_as(
            "SELECT id, code, name, account_type FROM account WHERE company_id = $1",
        )
        .bind(b.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let value_codes: Vec<(Uuid, String)> = match b.dimension_id {
            Some(d) => {
                sqlx::query_as("SELECT id, code FROM dimension_value WHERE dimension_id = $1")
                    .bind(d)
                    .fetch_all(&mut *ctx.conn)
                    .await?
            }
            None => vec![],
        };
        let mut rows = vec![];
        for ((account, month, value), (budget, actual)) in cells {
            let Some(a) = accounts.iter().find(|a| a.0 == account) else {
                continue;
            };
            if !["income", "expense"].contains(&a.3.as_str())
                && !budgeted_accounts.contains(&account)
            {
                continue;
            }
            let variance = actual - budget;
            let favourable = if a.3 == "income" {
                variance >= Decimal::ZERO
            } else {
                variance <= Decimal::ZERO
            };
            rows.push(ReportRow {
                account_id: account,
                code: a.1.clone(),
                name: a.2.clone(),
                account_type: a.3.clone(),
                month,
                dimension_value_id: value,
                dimension_value_code: value
                    .and_then(|v| value_codes.iter().find(|c| c.0 == v).map(|c| c.1.clone())),
                budget,
                actual,
                variance,
                variance_percent: (!budget.is_zero())
                    .then(|| (variance / budget.abs() * Decimal::ONE_HUNDRED).round_dp(2)),
                favourable,
            });
        }
        rows.sort_by(|x, y| {
            (x.month, &x.code, &x.dimension_value_code).cmp(&(
                y.month,
                &y.code,
                &y.dimension_value_code,
            ))
        });
        let mut months: Vec<Option<NaiveDate>> = rows.iter().map(|r| r.month).collect();
        months.sort();
        months.dedup();
        if months.is_empty() {
            months.push(None);
        }
        let totals = months
            .into_iter()
            .map(|month| {
                let sum = |t: &str| {
                    let mut totals = Totals::default();
                    for r in rows
                        .iter()
                        .filter(|r| r.month == month && r.account_type == t)
                    {
                        totals.budget += r.budget;
                        totals.actual += r.actual;
                        totals.variance += r.variance;
                    }
                    totals
                };
                let income = sum("income");
                let expense = sum("expense");
                let result = Totals {
                    budget: income.budget - expense.budget,
                    actual: income.actual - expense.actual,
                    variance: income.variance - expense.variance,
                };
                PeriodTotals {
                    month,
                    income,
                    expense,
                    result,
                }
            })
            .collect();
        Ok(ReportOutput {
            budget_id: b.id,
            from,
            to,
            currency,
            rows,
            totals,
        })
    }
}
