//! The key figures of a business as of a day.

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, JsonSchema, Result, Utc, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::common;
use crate::sales;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct KpiInput {
    /// The company to report on.
    company_id: Uuid,
    /// Today when omitted.
    as_of: Option<NaiveDate>,
    /// The days of sales and cost the figures are worked out over, up to
    /// as_of; 365 when omitted.
    days: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct KpiOutput {
    company_id: Uuid,
    as_of: NaiveDate,
    /// The first day of the days counted.
    from: NaiveDate,
    days: u32,
    currency: String,
    /// Net sales over the days, as posted.
    revenue: Decimal,
    /// The cost of those sales, as the stock ledger posted it.
    cost_of_sales: Decimal,
    gross_margin_percent: Option<Decimal>,
    /// The balance of the accounts the open receivables are kept on.
    receivables: Decimal,
    /// The balance of the accounts the open payables are kept on.
    payables: Decimal,
    /// The stock ledger's value.
    inventory: Decimal,
    /// Days sales outstanding: receivables over revenue, times the days.
    dso: Option<Decimal>,
    /// Days payables outstanding: payables over cost of sales, times the
    /// days.
    dpo: Option<Decimal>,
    /// Days inventory outstanding: inventory over cost of sales, times the
    /// days.
    dio: Option<Decimal>,
    /// Assets classed as cash or operating (accumulated depreciation left
    /// out) over liabilities classed as operating.
    current_assets: Decimal,
    current_liabilities: Decimal,
    current_ratio: Option<Decimal>,
    /// Open sales orders.
    open_orders: i64,
    /// Ordered and not yet delivered, at the lines' net price.
    backlog_to_deliver: Decimal,
    /// Delivered and not yet invoiced, at the lines' net price.
    backlog_to_invoice: Decimal,
}

pub(crate) struct Kpis;

#[async_trait]
impl Command for Kpis {
    type Input = KpiInput;
    type Output = KpiOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.kpis",
            "The key figures as of a day (today): revenue, cost of sales and gross margin over \
             the days before it (365), receivables, payables and inventory, DSO, DPO and DIO, \
             the current ratio (current assets and liabilities as the cash flow classification \
             of the accounts says), and the open sales order backlog, to deliver and to invoice. \
             All in the accounting currency, from what was posted.",
        )
        .report("Key figures")
        .permission("gl_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: KpiInput) -> Result<KpiOutput> {
        let company_id = input.company_id;
        ctx.require("gl_entry:read", company_id)?;
        let as_of = input.as_of.unwrap_or_else(|| Utc::now().date_naive());
        let days = input.days.unwrap_or(365).clamp(1, 3660);
        let from = as_of - chrono::Days::new(u64::from(days) - 1);
        let currency = company::base_currency(&mut *ctx.conn, company_id).await?;
        let (revenue, cost_of_sales) = if ctx.actor.can("sales_invoice:read", company_id) {
            let (net, cost) = sales::sales_totals(ctx, company_id, from, as_of).await?;
            let cost = if ctx.actor.can("stock_entry:read", company_id) {
                cost
            } else {
                Decimal::ZERO
            };
            (net, cost)
        } else {
            (Decimal::ZERO, Decimal::ZERO)
        };
        // Balances as of the day, debit minus credit.
        let control = |kind: &str| {
            format!(
                "SELECT COALESCE(sum(g.debit - g.credit), 0) FROM gl_entry g
                 WHERE g.company_id = $1 AND g.posting_date <= $2
                   AND g.account_id IN (SELECT DISTINCT account_id FROM open_item
                                        WHERE company_id = $1 AND kind = '{kind}')"
            )
        };
        let receivables: Decimal = sqlx::query_scalar(&control("receivable"))
            .bind(company_id)
            .bind(as_of)
            .fetch_one(&mut *ctx.conn)
            .await?;
        let payables: Decimal = sqlx::query_scalar::<_, Decimal>(&control("payable"))
            .bind(company_id)
            .bind(as_of)
            .fetch_one(&mut *ctx.conn)
            .await?;
        let payables = -payables;
        let inventory: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(sum(value), 0) FROM stock_entry
             WHERE company_id = $1 AND posting_date <= $2",
        )
        .bind(company_id)
        .bind(as_of)
        .fetch_one(&mut *ctx.conn)
        .await?;
        let (current_assets, current_liabilities): (Decimal, Decimal) = sqlx::query_as(
            "WITH a AS (
                SELECT a.id, a.account_type,
                       COALESCE(a.cash_flow,
                           CASE WHEN a.is_bank THEN 'cash'
                                WHEN EXISTS (SELECT 1 FROM account_rule r WHERE r.account_id = a.id
                                             AND r.purpose IN ('asset_cost', 'accumulated_depreciation'))
                                     THEN 'investing'
                                WHEN a.account_type = 'equity' THEN 'financing'
                                ELSE 'operating' END) AS flow
                FROM account a
                WHERE a.company_id = $1 AND a.account_type IN ('asset', 'liability'))
             SELECT COALESCE(sum(g.debit - g.credit) FILTER (
                        WHERE a.account_type = 'asset' AND a.flow IN ('cash', 'operating')), 0),
                    COALESCE(sum(g.credit - g.debit) FILTER (
                        WHERE a.account_type = 'liability' AND a.flow = 'operating'), 0)
             FROM gl_entry g JOIN a ON a.id = g.account_id
             WHERE g.company_id = $1 AND g.posting_date <= $2",
        )
        .bind(company_id)
        .bind(as_of)
        .fetch_one(&mut *ctx.conn)
        .await?;
        // The open orders, at their lines' net price in their currency,
        // then at the rate on record.
        let (mut open_orders, mut to_deliver, mut to_invoice) =
            (0i64, Decimal::ZERO, Decimal::ZERO);
        if ctx.actor.can("sales_order:read", company_id) {
            let lines: Vec<(Uuid, String, Decimal, Decimal, Decimal, Decimal)> = sqlx::query_as(
                "SELECT d.id, COALESCE(o.currency, co.base_currency),
                        l.qty, COALESCE(pr.qty_delivered, 0), COALESCE(pr.qty_invoiced, 0),
                        GREATEST(round(l.qty * l.unit_price, 4)
                                 * (1 - COALESCE(l.discount_percent, 0) / 100)
                                 - COALESCE(l.discount_amount, 0), 0)
                 FROM document d JOIN sales_order o ON o.document_id = d.id
                 JOIN company co ON co.id = d.company_id
                 JOIN sales_order_line l ON l.document_id = d.id
                 LEFT JOIN sales_order_progress pr ON pr.line_id = l.id
                 WHERE d.company_id = $1 AND d.status = 'open'",
            )
            .bind(company_id)
            .fetch_all(&mut *ctx.conn)
            .await?;
            let readable =
                common::readable_documents(ctx, lines.iter().map(|l| l.0).collect()).await?;
            open_orders = readable.len() as i64;
            let mut rates: std::collections::HashMap<String, Decimal> = Default::default();
            for (doc, line_currency, qty, delivered, invoiced, net) in lines {
                if !readable.contains(&doc) || qty.is_zero() {
                    continue;
                }
                let rate = match rates.get(&line_currency) {
                    Some(rate) => *rate,
                    None => {
                        let rate = if line_currency == currency {
                            Decimal::ONE
                        } else {
                            erpcraft_gl::rate_on_record(
                                &mut *ctx.conn,
                                &line_currency,
                                &currency,
                                as_of,
                            )
                            .await?
                            .unwrap_or_default()
                        };
                        rates.insert(line_currency.clone(), rate);
                        rate
                    }
                };
                to_deliver += net * (qty - delivered) / qty * rate;
                to_invoice += net * (delivered - invoiced) / qty * rate;
            }
        }
        let days_d = Decimal::from(days);
        let over = |a: Decimal, b: Decimal| common::ratio(a, b).map(|r| (r * days_d).round_dp(1));
        Ok(KpiOutput {
            company_id,
            as_of,
            from,
            days,
            currency,
            gross_margin_percent: common::percent(revenue - cost_of_sales, revenue),
            dso: over(receivables, revenue),
            dpo: over(payables, cost_of_sales),
            dio: over(inventory, cost_of_sales),
            current_ratio: common::ratio(current_assets, current_liabilities)
                .map(|r| r.round_dp(2)),
            revenue,
            cost_of_sales,
            receivables,
            payables,
            inventory,
            current_assets,
            current_liabilities,
            open_orders,
            backlog_to_deliver: to_deliver.round_dp(2),
            backlog_to_invoice: to_invoice.round_dp(2),
        })
    }
}
