//! The fixed asset register, tied to the ledger, and one asset's schedule.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, JsonSchema, Result, Utc, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::plan;
use crate::{ACCUMULATED, ASSET_COST, account, load_asset, params, settings};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegisterInput {
    /// The company to report on.
    company_id: Uuid,
    /// The day the register is as of. Today when omitted.
    as_of: Option<NaiveDate>,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct RegisterRow {
    asset_id: Uuid,
    code: String,
    name: String,
    class_id: Uuid,
    /// The class's code.
    class_code: String,
    status: String,
    in_service_date: Option<NaiveDate>,
    location: Option<String>,
    cost: Decimal,
    /// Depreciation up to the day, taken over depreciation included.
    accumulated: Decimal,
    /// Cost less accumulated depreciation.
    book_value: Decimal,
    /// Depreciation from the first of January of the day's year to the day.
    depreciation_this_year: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LedgerAccount {
    account_id: Uuid,
    code: String,
    name: String,
    /// What the register puts on the account: the cost of the assets it
    /// holds less the depreciation it holds, debit less credit.
    register: Decimal,
    /// The account's balance in the ledger as of the day, debit less
    /// credit.
    ledger: Decimal,
    /// Ledger less register: zero when they agree.
    difference: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RegisterOutput {
    company_id: Uuid,
    as_of: NaiveDate,
    /// The accounting currency, which every amount is in.
    currency: String,
    /// One row per asset in the books on the day, by code.
    assets: Vec<RegisterRow>,
    total_cost: Decimal,
    total_accumulated: Decimal,
    total_book_value: Decimal,
    total_depreciation_this_year: Decimal,
    /// The accounts the asset classes' account rules name for cost and
    /// accumulated depreciation, the register beside the ledger.
    accounts: Vec<LedgerAccount>,
    /// Whether every account agrees with the register.
    ties: bool,
}

pub(crate) struct Register;

#[async_trait]
impl Command for Register {
    type Input = RegisterInput;
    type Output = RegisterOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "asset.register",
            "The fixed asset register as of a day: per asset its cost, accumulated depreciation, \
             book value and this year's depreciation; and the accounts that hold them, the \
             register beside the ledger, so that a difference shows. Takes asset:read.",
        )
        .permission("asset:read")
        .label("Fixed asset register")
        .resource("asset")
        .target(false)
        .report("Fixed asset register")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: RegisterInput) -> Result<RegisterOutput> {
        ctx.require("asset:read", input.company_id)?;
        let as_of = input.as_of.unwrap_or_else(|| Utc::now().date_naive());
        let year_start = NaiveDate::from_ymd_opt(as_of.year(), 1, 1).unwrap_or(as_of);
        let currency =
            erpcraft_kernel::company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let assets: Vec<RegisterRow> = sqlx::query_as(
            "SELECT a.id AS asset_id, a.code, a.name, a.class_id, c.code AS class_code, a.status,
                    a.in_service_date, a.location, a.cost,
                    a.opening_accumulated + COALESCE(d.upto, 0) AS accumulated,
                    a.cost - a.opening_accumulated - COALESCE(d.upto, 0) AS book_value,
                    COALESCE(d.this_year, 0) AS depreciation_this_year
             FROM asset a JOIN asset_class c ON c.id = a.class_id
             LEFT JOIN LATERAL (
                 SELECT sum(amount) AS upto,
                        sum(amount) FILTER (WHERE posting_date >= $3) AS this_year
                 FROM asset_depreciation
                 WHERE asset_id = a.id AND NOT reversed AND posting_date <= $2
             ) d ON true
             WHERE a.company_id = $1 AND a.cost IS NOT NULL AND a.recorded_on <= $2
               AND (a.disposed_on IS NULL OR a.disposed_on > $2)
             ORDER BY a.code",
        )
        .bind(input.company_id)
        .bind(as_of)
        .bind(year_start)
        .fetch_all(&mut *ctx.conn)
        .await?;

        // What the register puts on each account.
        let mut register: BTreeMap<Uuid, Decimal> = BTreeMap::new();
        let mut accounts_of: BTreeMap<Uuid, (Uuid, Uuid)> = BTreeMap::new();
        for row in &assets {
            let (cost, accumulated) = match accounts_of.get(&row.class_id) {
                Some(pair) => *pair,
                None => {
                    let cost =
                        account(&mut *ctx.conn, input.company_id, ASSET_COST, row.class_id).await?;
                    let accumulated =
                        account(&mut *ctx.conn, input.company_id, ACCUMULATED, row.class_id)
                            .await?;
                    accounts_of.insert(row.class_id, (cost, accumulated));
                    (cost, accumulated)
                }
            };
            *register.entry(cost).or_default() += row.cost;
            *register.entry(accumulated).or_default() -= row.accumulated;
        }
        // Every account a rule names for either purpose, assets or not.
        let named: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT account_id FROM account_rule
             WHERE company_id = $1 AND purpose IN ($2, $3)",
        )
        .bind(input.company_id)
        .bind(ASSET_COST)
        .bind(ACCUMULATED)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let ids: BTreeSet<Uuid> = named.into_iter().chain(register.keys().copied()).collect();
        let ids: Vec<Uuid> = ids.into_iter().collect();
        let balances: Vec<(Uuid, String, String, Decimal)> = sqlx::query_as(
            "SELECT a.id, a.code, a.name,
                    COALESCE((SELECT sum(g.debit - g.credit) FROM gl_entry g
                              WHERE g.account_id = a.id AND g.posting_date <= $2), 0)
             FROM account a WHERE a.id = ANY($1) ORDER BY a.code",
        )
        .bind(&ids)
        .bind(as_of)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let accounts: Vec<LedgerAccount> = balances
            .into_iter()
            .map(|(account_id, code, name, ledger)| {
                let register = register.get(&account_id).copied().unwrap_or_default();
                LedgerAccount {
                    account_id,
                    code,
                    name,
                    register,
                    ledger,
                    difference: ledger - register,
                }
            })
            .collect();
        let sum = |f: fn(&RegisterRow) -> Decimal| assets.iter().map(f).sum::<Decimal>();
        Ok(RegisterOutput {
            company_id: input.company_id,
            as_of,
            currency,
            total_cost: sum(|r| r.cost),
            total_accumulated: sum(|r| r.accumulated),
            total_book_value: sum(|r| r.book_value),
            total_depreciation_this_year: sum(|r| r.depreciation_this_year),
            ties: accounts.iter().all(|a| a.difference.is_zero()),
            assets,
            accounts,
        })
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScheduleInput {
    /// Asset id.
    id: Uuid,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ScheduleRow {
    /// The first day of the month depreciated.
    month: NaiveDate,
    amount: Decimal,
    /// Depreciation up to and including this month.
    accumulated: Decimal,
    book_value: Decimal,
    /// Posted, by the document named; otherwise planned.
    posted: bool,
    /// The document that posted the month: its id, type and number.
    document_id: Option<Uuid>,
    /// `depreciation_run`, or `asset_disposal` for the months a disposal
    /// caught up to its date.
    doc_type: Option<String>,
    number: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ScheduleOutput {
    asset_id: Uuid,
    code: String,
    name: String,
    status: String,
    method: String,
    useful_life_months: Option<i32>,
    /// Percent a year, for the declining balance.
    declining_rate: Option<Decimal>,
    cost: Option<Decimal>,
    salvage_value: Decimal,
    in_service_date: Option<NaiveDate>,
    /// Depreciation taken over from another system, up to the end of the
    /// month of `opening_through`.
    opening_accumulated: Decimal,
    opening_through: Option<NaiveDate>,
    /// Month by month: what was posted, then what is planned.
    months: Vec<ScheduleRow>,
}

pub(crate) struct Schedule;

#[async_trait]
impl Command for Schedule {
    type Input = ScheduleInput;
    type Output = ScheduleOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "asset.schedule",
            "One asset's depreciation month by month: what has been posted, and what is \
             planned to the end of its life, with the book value after each month. Takes \
             asset:read.",
        )
        .permission("asset:read")
        .label("Depreciation schedule")
        .resource("asset")
        .report("Depreciation schedule")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ScheduleInput) -> Result<ScheduleOutput> {
        let asset = load_asset(&mut *ctx.conn, input.id, false).await?;
        ctx.require("asset:read", asset.company_id)?;
        let posted: Vec<(NaiveDate, Decimal, Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT d.month, d.amount, d.document_id, doc.doc_type, doc.number
             FROM asset_depreciation d
             JOIN document doc ON doc.id = d.document_id
             WHERE d.asset_id = $1 AND NOT d.reversed ORDER BY d.month",
        )
        .bind(asset.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let done: BTreeSet<NaiveDate> = posted.iter().map(|(m, ..)| *m).collect();
        type By = (Uuid, String, Option<String>);
        let mut rows: Vec<(NaiveDate, Decimal, Option<By>)> = posted
            .into_iter()
            .map(|(m, a, d, t, n)| (m, a, Some((d, t, n))))
            .collect();
        if asset.status != "disposed" {
            let settings = settings(&mut *ctx.conn, asset.company_id).await?;
            let places =
                erpcraft_kernel::company::currency_decimals(&mut *ctx.conn, asset.company_id)
                    .await?;
            if let Some(p) = params(&asset, &settings, places) {
                rows.extend(
                    plan::plan(&p)
                        .into_iter()
                        .filter(|(m, _)| !done.contains(m))
                        .map(|(m, a)| (m, a, None)),
                );
            }
        }
        rows.sort_by_key(|(m, ..)| *m);
        let cost = asset.cost.unwrap_or_default();
        let mut accumulated = asset.opening_accumulated;
        let months = rows
            .into_iter()
            .map(|(month, amount, by)| {
                accumulated += amount;
                ScheduleRow {
                    month,
                    amount,
                    accumulated,
                    book_value: cost - accumulated,
                    posted: by.is_some(),
                    document_id: by.as_ref().map(|(d, ..)| *d),
                    doc_type: by.as_ref().map(|(_, t, _)| t.clone()),
                    number: by.and_then(|(.., n)| n),
                }
            })
            .collect();
        Ok(ScheduleOutput {
            asset_id: asset.id,
            code: asset.code,
            name: asset.name,
            status: asset.status,
            method: asset.method,
            useful_life_months: asset.useful_life_months,
            declining_rate: asset.declining_rate,
            cost: asset.cost,
            salvage_value: asset.salvage_value,
            in_service_date: asset.in_service_date,
            opening_accumulated: asset.opening_accumulated,
            opening_through: asset.opening_through,
            months,
        })
    }
}
