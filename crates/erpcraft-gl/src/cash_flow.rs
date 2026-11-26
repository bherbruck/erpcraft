//! The cash flow statement by the indirect method: the net income, and the
//! change of every balance sheet account that is not cash, by the flow it
//! belongs to. Since every posting balances, they add up to the change in
//! cash.

use chrono::{Datelike, Months, NaiveDate};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const READ: &str = "gl_entry:read";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CashFlowInput {
    /// The company to report on.
    company_id: Uuid,
    /// First posting date of the range.
    from: NaiveDate,
    /// Last posting date of the range.
    to: NaiveDate,
    /// A column per month of the range rather than one for all of it.
    #[serde(default)]
    by_month: bool,
    /// Add the same columns a year earlier, for comparison.
    #[serde(default)]
    compare: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Column {
    from: NaiveDate,
    to: NaiveDate,
    /// Income minus expense.
    net_income: Decimal,
    operating: Decimal,
    investing: Decimal,
    financing: Decimal,
    /// Operating, investing and financing together.
    net_change: Decimal,
    cash_at_start: Decimal,
    cash_at_end: Decimal,
    /// The cash at the start plus the net change is the cash at the end.
    reconciles: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct Row {
    account_id: Uuid,
    code: String,
    name: String,
    /// What the account's change did to cash, per column: an increase of an
    /// asset is below zero, an increase of a liability or equity above.
    amounts: Vec<Decimal>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CashFlowOutput {
    company_id: Uuid,
    currency: String,
    /// The periods, in order; with `compare`, those of a year earlier
    /// follow.
    columns: Vec<Column>,
    /// The changes of operating accounts, after the net income.
    operating: Vec<Row>,
    investing: Vec<Row>,
    financing: Vec<Row>,
}

pub(crate) struct CashFlow;

/// The months of a range, the first and last cut to it.
fn months(from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out = vec![];
    let mut start = from;
    while start <= to {
        let first = start.with_day(1).unwrap_or(start);
        let end = first
            .checked_add_months(Months::new(1))
            .and_then(|d| d.pred_opt())
            .unwrap_or(to)
            .min(to);
        out.push((start, end));
        match end.succ_opt() {
            Some(next) => start = next,
            None => break,
        }
    }
    out
}

#[async_trait]
impl Command for CashFlow {
    type Input = CashFlowInput;
    type Output = CashFlowOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "gl.cash_flow",
            "Cash flow statement of a company by the indirect method, in its accounting currency: \
             net income, then the change of each operating account (receivables, payables, \
             accumulated depreciation, ...), of each investing account (fixed assets) and of \
             each financing account (loans, equity), signed as they moved cash; the net change \
             in cash, the cash at the start and at the end, and whether they reconcile. An \
             account's cash_flow says which flow it is part of; unset, a bank or cash account is \
             cash, the cost account of a fixed asset class investing, other assets and \
             liabilities operating, equity financing. by_month gives a column per month; \
             compare adds the same columns a year earlier. Year-end closing entries are left \
             out.",
        )
        .report("Cash flow statement")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CashFlowInput) -> Result<CashFlowOutput> {
        ctx.require(READ, input.company_id)?;
        if input.from > input.to {
            return Err(Error::validation("`from` cannot be after `to`"));
        }
        let mut periods = if input.by_month {
            months(input.from, input.to)
        } else {
            vec![(input.from, input.to)]
        };
        if periods.len() > 36 {
            return Err(Error::validation(
                "a cash flow statement has at most 36 columns",
            ));
        }
        if input.compare {
            let earlier: Vec<(NaiveDate, NaiveDate)> = periods
                .iter()
                .filter_map(|(f, t)| {
                    // A period that ends on the last of a month ends on the
                    // last of that month a year earlier: the 29th of a
                    // February too.
                    let after = t.succ_opt()?;
                    let to = if after.day() == 1 {
                        after.checked_sub_months(Months::new(12))?.pred_opt()?
                    } else {
                        t.checked_sub_months(Months::new(12))?
                    };
                    Some((f.checked_sub_months(Months::new(12))?, to))
                })
                .collect();
            periods.extend(earlier);
        }
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        // Every account of the company with the flow it is part of.
        let accounts: Vec<(Uuid, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT a.id, a.code, a.name, a.account_type,
                    COALESCE(a.cash_flow,
                        CASE WHEN a.account_type IN ('income', 'expense') THEN NULL
                             WHEN a.is_bank THEN 'cash'
                             WHEN EXISTS (SELECT 1 FROM account_rule r
                                          WHERE r.account_id = a.id AND r.purpose = 'asset_cost')
                                  THEN 'investing'
                             WHEN a.account_type = 'equity' THEN 'financing'
                             ELSE 'operating' END)
             FROM account a WHERE a.company_id = $1 AND NOT a.is_group ORDER BY a.code",
        )
        .bind(input.company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let mut columns = vec![];
        let mut rows: Vec<(String, Row)> = vec![];
        for (n, (from, to)) in periods.iter().enumerate() {
            // Debit minus credit per account over the period, closing
            // entries left out.
            let changes: Vec<(Uuid, Decimal)> = sqlx::query_as(
                "SELECT g.account_id, sum(g.debit) - sum(g.credit)
                 FROM gl_entry g JOIN posting p ON p.id = g.posting_id
                 WHERE g.company_id = $1 AND g.posting_date BETWEEN $2 AND $3
                   AND NOT starts_with(p.command, 'year_end_close.')
                 GROUP BY g.account_id",
            )
            .bind(input.company_id)
            .bind(from)
            .bind(to)
            .fetch_all(&mut *ctx.conn)
            .await?;
            let cash_before: Decimal = sqlx::query_scalar(
                "SELECT COALESCE(sum(g.debit) - sum(g.credit), 0)
                 FROM gl_entry g JOIN account a ON a.id = g.account_id
                 WHERE g.company_id = $1 AND g.posting_date < $2
                   AND COALESCE(a.cash_flow,
                           CASE WHEN a.account_type IN ('income', 'expense') THEN NULL
                                WHEN a.is_bank THEN 'cash' END) = 'cash'",
            )
            .bind(input.company_id)
            .bind(from)
            .fetch_one(&mut *ctx.conn)
            .await?;
            let mut column = Column {
                from: *from,
                to: *to,
                net_income: Decimal::ZERO,
                operating: Decimal::ZERO,
                investing: Decimal::ZERO,
                financing: Decimal::ZERO,
                net_change: Decimal::ZERO,
                cash_at_start: cash_before,
                cash_at_end: cash_before,
                reconciles: false,
            };
            for (id, change) in &changes {
                let Some(account) = accounts.iter().find(|a| a.0 == *id) else {
                    continue;
                };
                // What it did to cash: the opposite of its debit balance's
                // change.
                let effect = -*change;
                match account.4.as_deref() {
                    None => column.net_income += effect,
                    Some("cash") => column.cash_at_end += *change,
                    Some(flow) => {
                        match flow {
                            "investing" => column.investing += effect,
                            "financing" => column.financing += effect,
                            _ => column.operating += effect,
                        }
                        let row = match rows.iter_mut().find(|r| r.1.account_id == *id) {
                            Some((_, row)) => row,
                            None => {
                                rows.push((
                                    flow.to_owned(),
                                    Row {
                                        account_id: *id,
                                        code: account.1.clone(),
                                        name: account.2.clone(),
                                        amounts: vec![Decimal::ZERO; periods.len()],
                                    },
                                ));
                                &mut rows.last_mut().expect("just pushed").1
                            }
                        };
                        row.amounts[n] = effect;
                    }
                }
            }
            column.operating += column.net_income;
            column.net_change = column.operating + column.investing + column.financing;
            column.reconciles = column.cash_at_start + column.net_change == column.cash_at_end;
            columns.push(column);
        }
        rows.sort_by(|a, b| a.1.code.cmp(&b.1.code));
        let take = |flow: &str, rows: &mut Vec<(String, Row)>| -> Vec<Row> {
            let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(rows)
                .into_iter()
                .partition(|(f, _)| f == flow);
            *rows = rest;
            mine.into_iter().map(|(_, r)| r).collect()
        };
        let operating = take("operating", &mut rows);
        let investing = take("investing", &mut rows);
        let financing = take("financing", &mut rows);
        Ok(CashFlowOutput {
            company_id: input.company_id,
            currency,
            columns,
            operating,
            investing,
            financing,
        })
    }
}
