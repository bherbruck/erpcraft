//! The stock reports: aging, slow movers and dead stock, and turnover.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, JsonSchema, Result, Utc, async_trait, company, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::common;

const READ: &str = "stock_entry:read";

/// Entries that moved goods between warehouses: neither a receipt from
/// outside nor an issue.
const TRANSFERS: &str = "('stock_transfer', 'stock_transfer_receipt')";

/// How a stock report is aged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgeBy {
    /// Each item's level in a warehouse, first in, first out.
    #[default]
    Level,
    /// Each lot in a warehouse, from the day it first came in.
    Lot,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgingInput {
    company_id: Uuid,
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "warehouse", "key": "id" }))]
    warehouse_id: Option<Uuid>,
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "item", "key": "id" }))]
    item_id: Option<Uuid>,
    /// level (the default) or lot.
    #[serde(default)]
    by: AgeBy,
}

/// The stock on hand of an item in a warehouse (and a lot), by age.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub(crate) struct AgingRow {
    item_id: Uuid,
    item_code: Option<String>,
    item_name: Option<String>,
    warehouse_id: Uuid,
    warehouse_code: Option<String>,
    lot_id: Option<Uuid>,
    lot_code: Option<String>,
    /// In the item's stock unit.
    qty: Decimal,
    /// At the level's cost, in the accounting currency.
    value: Decimal,
    oldest_receipt: Option<NaiveDate>,
    /// Weighted by quantity.
    average_age_days: Option<Decimal>,
    value_0_30: Decimal,
    value_31_60: Decimal,
    value_61_90: Decimal,
    value_91_180: Decimal,
    value_over_180: Decimal,
}

impl AgingRow {
    /// Put what rounding the buckets left over in the largest, so that
    /// they add up to the value.
    fn settle(&mut self) {
        let left = self.value
            - (self.value_0_30
                + self.value_31_60
                + self.value_61_90
                + self.value_91_180
                + self.value_over_180);
        if left.is_zero() {
            return;
        }
        let largest = [
            &mut self.value_0_30,
            &mut self.value_31_60,
            &mut self.value_61_90,
            &mut self.value_91_180,
            &mut self.value_over_180,
        ]
        .into_iter()
        .max_by_key(|v| **v);
        if let Some(bucket) = largest {
            *bucket += left;
        }
    }

    /// Count `qty` of the row as received `age` days ago, at `unit` each.
    fn add(&mut self, qty: Decimal, age: i64, unit: Decimal) {
        let value = (qty * unit).round_dp(2);
        match age {
            ..=30 => self.value_0_30 += value,
            31..=60 => self.value_31_60 += value,
            61..=90 => self.value_61_90 += value,
            91..=180 => self.value_91_180 += value,
            _ => self.value_over_180 += value,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AgingOutput {
    company_id: Uuid,
    as_of: NaiveDate,
    currency: String,
    rows: Vec<AgingRow>,
    totals: AgingRow,
}

pub(crate) struct Aging;

#[async_trait]
impl Command for Aging {
    type Input = AgingInput;
    type Output = AgingOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.inventory_aging",
            "The stock on hand today by how long ago it came in: per item and warehouse, first \
             in first out (what is on hand is what came in last), or per lot from the day it \
             first came into the warehouse (by lot). Value at the level's cost, in buckets of \
             0-30, 31-60, 61-90, 91-180 and over 180 days, with the oldest receipt and the \
             average age.",
        )
        .report("Inventory aging")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: AgingInput) -> Result<AgingOutput> {
        ctx.require(READ, input.company_id)?;
        let today = Utc::now().date_naive();
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let levels: Vec<(Uuid, Uuid, Decimal, Decimal)> = sqlx::query_as(
            "SELECT item_id, warehouse_id, qty, value FROM stock_level
             WHERE company_id = $1 AND qty > 0
               AND ($2::uuid IS NULL OR warehouse_id = $2) AND ($3::uuid IS NULL OR item_id = $3)
             ORDER BY item_id, warehouse_id",
        )
        .bind(input.company_id)
        .bind(input.warehouse_id)
        .bind(input.item_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let unit: HashMap<(Uuid, Uuid), Decimal> = levels
            .iter()
            .map(|(i, w, q, v)| ((*i, *w), v / q))
            .collect();
        let mut rows: Vec<AgingRow> = vec![];
        match input.by {
            AgeBy::Level => {
                // Receipts that stand, newest first.
                // Receipts that stand, newest first. A transfer in is a
                // receipt of the warehouse it came into: age is age there.
                let receipts: Vec<(Uuid, Uuid, NaiveDate, Decimal)> = sqlx::query_as(
                    "SELECT se.item_id, se.warehouse_id, se.posting_date, se.qty
                     FROM stock_entry se
                     WHERE se.company_id = $1 AND se.qty > 0 AND se.reverses_id IS NULL
                       AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = se.id)
                       AND ($2::uuid IS NULL OR se.warehouse_id = $2)
                       AND ($3::uuid IS NULL OR se.item_id = $3)
                     ORDER BY se.item_id, se.warehouse_id, se.posting_date DESC, se.created_at DESC",
                )
                .bind(input.company_id)
                .bind(input.warehouse_id)
                .bind(input.item_id)
                .fetch_all(&mut *ctx.conn)
                .await?;
                let mut by_level: HashMap<(Uuid, Uuid), Vec<(NaiveDate, Decimal)>> = HashMap::new();
                for (item, warehouse, date, qty) in receipts {
                    by_level
                        .entry((item, warehouse))
                        .or_default()
                        .push((date, qty));
                }
                for (item, warehouse, qty, value) in &levels {
                    let unit = unit[&(*item, *warehouse)];
                    let mut row = AgingRow {
                        item_id: *item,
                        warehouse_id: *warehouse,
                        qty: *qty,
                        value: *value,
                        ..Default::default()
                    };
                    let mut left = *qty;
                    let mut weighted = Decimal::ZERO;
                    for (date, received) in by_level.get(&(*item, *warehouse)).into_iter().flatten()
                    {
                        if left <= Decimal::ZERO {
                            break;
                        }
                        let take = left.min(*received);
                        let age = (today - *date).num_days();
                        row.add(take, age, unit);
                        weighted += take * Decimal::from(age);
                        row.oldest_receipt = Some(*date);
                        left -= take;
                    }
                    // What no receipt on record explains (stock from before
                    // the ledger, or below zero once) is counted as old.
                    if left > Decimal::ZERO {
                        row.add(left, i64::MAX, unit);
                    }
                    row.average_age_days = (left.is_zero()).then(|| (weighted / *qty).round_dp(1));
                    rows.push(row);
                }
            }
            AgeBy::Lot => {
                let lots: Vec<(Uuid, String, Uuid, Uuid, Decimal, Option<NaiveDate>)> =
                    sqlx::query_as(
                        "SELECT ll.lot_id, l.code, ll.item_id, ll.warehouse_id, ll.qty,
                                (SELECT min(se.posting_date) FROM stock_entry_lot el
                                 JOIN stock_entry se ON se.id = el.entry_id
                                 WHERE el.lot_id = ll.lot_id AND se.warehouse_id = ll.warehouse_id
                                   AND el.qty > 0)
                         FROM stock_lot_level ll JOIN stock_lot l ON l.id = ll.lot_id
                         WHERE ll.company_id = $1 AND ll.qty > 0
                           AND ($2::uuid IS NULL OR ll.warehouse_id = $2)
                           AND ($3::uuid IS NULL OR ll.item_id = $3)
                         ORDER BY ll.item_id, ll.warehouse_id, l.code",
                    )
                    .bind(input.company_id)
                    .bind(input.warehouse_id)
                    .bind(input.item_id)
                    .fetch_all(&mut *ctx.conn)
                    .await?;
                for (lot, code, item, warehouse, qty, first) in lots {
                    let unit = unit.get(&(item, warehouse)).copied().unwrap_or_default();
                    let age = first.map(|d| (today - d).num_days());
                    let mut row = AgingRow {
                        item_id: item,
                        warehouse_id: warehouse,
                        lot_id: Some(lot),
                        lot_code: Some(code),
                        qty,
                        value: (qty * unit).round_dp(2),
                        oldest_receipt: first,
                        average_age_days: age.map(Decimal::from),
                        ..Default::default()
                    };
                    row.add(qty, age.unwrap_or(i64::MAX), unit);
                    rows.push(row);
                }
            }
        }
        let items = common::labels(
            ctx,
            "item",
            "item",
            &rows.iter().map(|r| r.item_id).collect(),
        )
        .await?;
        let warehouses = common::labels(
            ctx,
            "warehouse",
            "warehouse",
            &rows.iter().map(|r| r.warehouse_id).collect(),
        )
        .await?;
        let mut totals = AgingRow::default();
        for row in &mut rows {
            row.settle();
            row.qty = row.qty.normalize();
            (row.item_code, row.item_name) = common::label(&items, Some(row.item_id));
            row.warehouse_code = common::label(&warehouses, Some(row.warehouse_id)).0;
            totals.qty += row.qty;
            totals.value += row.value;
            totals.value_0_30 += row.value_0_30;
            totals.value_31_60 += row.value_31_60;
            totals.value_61_90 += row.value_61_90;
            totals.value_91_180 += row.value_91_180;
            totals.value_over_180 += row.value_over_180;
        }
        Ok(AgingOutput {
            company_id: input.company_id,
            as_of: today,
            currency,
            rows,
            totals,
        })
    }
}

/// How a level with stock is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Movement {
    /// Nothing issued in the days looked at.
    Dead,
    /// Issued, but what is on hand lasts longer than slow_cover_days.
    Slow,
    Moving,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlowInput {
    company_id: Uuid,
    /// The days looked back over; 90 when omitted.
    days: Option<u32>,
    /// Longer cover than this is slow; 180 days when omitted.
    slow_cover_days: Option<u32>,
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "warehouse", "key": "id" }))]
    warehouse_id: Option<Uuid>,
    /// Every level with stock, the moving ones too.
    #[serde(default)]
    all: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SlowRow {
    item_id: Uuid,
    item_code: Option<String>,
    item_name: Option<String>,
    warehouse_id: Uuid,
    warehouse_code: Option<String>,
    movement: Movement,
    qty: Decimal,
    value: Decimal,
    last_issue: Option<NaiveDate>,
    days_since_issue: Option<i64>,
    /// In the days looked back over.
    issued_qty: Decimal,
    /// How long what is on hand lasts at that rate.
    days_of_cover: Option<Decimal>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SlowOutput {
    company_id: Uuid,
    as_of: NaiveDate,
    days: u32,
    currency: String,
    rows: Vec<SlowRow>,
    /// The value of the dead stock, and of the slow.
    dead_value: Decimal,
    slow_value: Decimal,
}

pub(crate) struct SlowMovers;

#[async_trait]
impl Command for SlowMovers {
    type Input = SlowInput;
    type Output = SlowOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.slow_movers",
            "Slow movers and dead stock: each item's level in a warehouse with stock, its last \
             issue, what was issued in the last `days` (90) days and how many days what is on \
             hand lasts at that rate. dead: nothing issued in those days; slow: the cover is \
             longer than slow_cover_days (180). Issues are entries that took goods out and stand, \
             transfers to another warehouse left out. Only dead and slow levels unless `all`.",
        )
        .report("Slow movers and dead stock")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SlowInput) -> Result<SlowOutput> {
        ctx.require(READ, input.company_id)?;
        let today = Utc::now().date_naive();
        let days = input.days.unwrap_or(90).max(1);
        let slow = Decimal::from(input.slow_cover_days.unwrap_or(180));
        let since = today - chrono::Days::new(u64::from(days));
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let levels: Vec<(Uuid, Uuid, Decimal, Decimal, Option<NaiveDate>, Decimal)> =
            sqlx::query_as(&format!(
                "WITH issues AS (
                    SELECT se.item_id, se.warehouse_id, se.posting_date, -se.qty AS qty
                    FROM stock_entry se JOIN posting p ON p.id = se.posting_id
                    JOIN document d ON d.id = p.document_id
                    WHERE se.company_id = $1 AND se.qty < 0 AND se.reverses_id IS NULL
                      AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = se.id)
                      AND d.doc_type NOT IN {TRANSFERS})
                 SELECT l.item_id, l.warehouse_id, l.qty, l.value,
                        (SELECT max(i.posting_date) FROM issues i
                         WHERE i.item_id = l.item_id AND i.warehouse_id = l.warehouse_id),
                        (SELECT COALESCE(sum(i.qty), 0) FROM issues i
                         WHERE i.item_id = l.item_id AND i.warehouse_id = l.warehouse_id
                           AND i.posting_date > $2)
                 FROM stock_level l
                 WHERE l.company_id = $1 AND l.qty > 0 AND ($3::uuid IS NULL OR l.warehouse_id = $3)
                 ORDER BY l.value DESC"
            ))
            .bind(input.company_id)
            .bind(since)
            .bind(input.warehouse_id)
            .fetch_all(&mut *ctx.conn)
            .await?;
        let mut rows: Vec<SlowRow> = levels
            .into_iter()
            .map(|(item, warehouse, qty, value, last, issued)| {
                let cover =
                    (!issued.is_zero()).then(|| (qty * Decimal::from(days) / issued).round_dp(0));
                let movement = match cover {
                    None => Movement::Dead,
                    Some(c) if c > slow => Movement::Slow,
                    Some(_) => Movement::Moving,
                };
                SlowRow {
                    item_id: item,
                    item_code: None,
                    item_name: None,
                    warehouse_id: warehouse,
                    warehouse_code: None,
                    movement,
                    qty,
                    value,
                    last_issue: last,
                    days_since_issue: last.map(|d| (today - d).num_days()),
                    issued_qty: issued,
                    days_of_cover: cover,
                }
            })
            .filter(|r| input.all || r.movement != Movement::Moving)
            .collect();
        let items = common::labels(
            ctx,
            "item",
            "item",
            &rows.iter().map(|r| r.item_id).collect(),
        )
        .await?;
        let warehouses = common::labels(
            ctx,
            "warehouse",
            "warehouse",
            &rows.iter().map(|r| r.warehouse_id).collect(),
        )
        .await?;
        let (mut dead_value, mut slow_value) = (Decimal::ZERO, Decimal::ZERO);
        for row in &mut rows {
            (row.item_code, row.item_name) = common::label(&items, Some(row.item_id));
            row.warehouse_code = common::label(&warehouses, Some(row.warehouse_id)).0;
            match row.movement {
                Movement::Dead => dead_value += row.value,
                Movement::Slow => slow_value += row.value,
                Movement::Moving => {}
            }
        }
        Ok(SlowOutput {
            company_id: input.company_id,
            as_of: today,
            days,
            currency,
            rows,
            dead_value,
            slow_value,
        })
    }
}

/// What turnover is shown per.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnoverBy {
    #[default]
    Item,
    ItemGroup,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnoverInput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "warehouse", "key": "id" }))]
    warehouse_id: Option<Uuid>,
    /// item (the default) or item_group.
    #[serde(default)]
    group_by: TurnoverBy,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub(crate) struct TurnoverRow {
    item_id: Option<Uuid>,
    item_code: Option<String>,
    item_name: Option<String>,
    item_group_id: Option<Uuid>,
    item_group_code: Option<String>,
    item_group_name: Option<String>,
    /// What was issued in the range, at the cost it left at, net of
    /// reversals.
    cost_of_issues: Decimal,
    opening_value: Decimal,
    closing_value: Decimal,
    /// Of the opening and closing value.
    average_value: Decimal,
    /// Cost of issues over average value.
    turnover: Option<Decimal>,
    /// The days of the range over the turnover.
    days_in_inventory: Option<Decimal>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct TurnoverOutput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    currency: String,
    rows: Vec<TurnoverRow>,
    totals: TurnoverRow,
}

pub(crate) struct Turnover;

#[async_trait]
impl Command for Turnover {
    type Input = TurnoverInput;
    type Output = TurnoverOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "analysis.inventory_turnover",
            "Inventory turnover over a range, per item or item group: the cost of what left \
             stock (issues that stand, at the cost the stock ledger posted, transfers left out), \
             the stock value at the start and the end and their average, the turnover (cost over \
             average value) and the days in inventory.",
        )
        .report("Inventory turnover")
        .permission(READ)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TurnoverInput) -> Result<TurnoverOutput> {
        ctx.require(READ, input.company_id)?;
        common::check_range(input.from, input.to)?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let found: Vec<(Uuid, Decimal, Decimal, Decimal)> = sqlx::query_as(&format!(
            "SELECT se.item_id,
                    COALESCE(sum(-se.value) FILTER (
                        WHERE se.posting_date BETWEEN $2 AND $3 AND d.doc_type NOT IN {TRANSFERS}
                          AND ((se.qty < 0 AND se.reverses_id IS NULL)
                               OR se.reverses_id IN (SELECT o.id FROM stock_entry o
                                                     WHERE o.qty < 0))), 0),
                    COALESCE(sum(se.value) FILTER (WHERE se.posting_date < $2), 0),
                    COALESCE(sum(se.value) FILTER (WHERE se.posting_date <= $3), 0)
             FROM stock_entry se JOIN posting p ON p.id = se.posting_id
             JOIN document d ON d.id = p.document_id
             WHERE se.company_id = $1 AND se.posting_date <= $3
               AND ($4::uuid IS NULL OR se.warehouse_id = $4)
             GROUP BY se.item_id"
        ))
        .bind(input.company_id)
        .bind(input.from)
        .bind(input.to)
        .bind(input.warehouse_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let days = Decimal::from((input.to - input.from).num_days() + 1);
        let groups = if input.group_by == TurnoverBy::ItemGroup {
            common::groups_of(ctx, &found.iter().map(|f| f.0).collect()).await?
        } else {
            HashMap::new()
        };
        let mut by: BTreeMap<(Option<Uuid>, Option<Uuid>), (Decimal, Decimal, Decimal)> =
            BTreeMap::new();
        for (item, cost, opening, closing) in &found {
            if cost.is_zero() && opening.is_zero() && closing.is_zero() {
                continue;
            }
            let key = match input.group_by {
                TurnoverBy::Item => (Some(*item), None),
                TurnoverBy::ItemGroup => (None, groups.get(item).copied()),
            };
            let sums = by.entry(key).or_default();
            sums.0 += cost;
            sums.1 += opening;
            sums.2 += closing;
        }
        let figures =
            |row: &mut TurnoverRow, (cost, opening, closing): (Decimal, Decimal, Decimal)| {
                row.cost_of_issues = cost;
                row.opening_value = opening;
                row.closing_value = closing;
                row.average_value = ((opening + closing) / Decimal::TWO).round_dp(2);
                row.turnover = common::ratio(cost, row.average_value).map(|t| t.round_dp(2));
                row.days_in_inventory = row
                    .turnover
                    .filter(|t| !t.is_zero())
                    .map(|t| (days / t).round_dp(1));
            };
        let items = common::labels(
            ctx,
            "item",
            "item",
            &by.keys().filter_map(|k| k.0).collect::<BTreeSet<_>>(),
        )
        .await?;
        let group_labels = common::labels(
            ctx,
            "price_group",
            "price_group",
            &by.keys().filter_map(|k| k.1).collect::<BTreeSet<_>>(),
        )
        .await?;
        let mut total = (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
        let mut rows: Vec<TurnoverRow> = by
            .into_iter()
            .map(|((item, group), sums)| {
                total.0 += sums.0;
                total.1 += sums.1;
                total.2 += sums.2;
                let mut row = TurnoverRow {
                    item_id: item,
                    item_group_id: group,
                    ..Default::default()
                };
                (row.item_code, row.item_name) = common::label(&items, item);
                (row.item_group_code, row.item_group_name) = common::label(&group_labels, group);
                figures(&mut row, sums);
                row
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.cost_of_issues));
        let mut totals = TurnoverRow::default();
        figures(&mut totals, total);
        Ok(TurnoverOutput {
            company_id: input.company_id,
            from: input.from,
            to: input.to,
            currency,
            rows,
            totals,
        })
    }
}
