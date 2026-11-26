//! What the reports share: ranges and their comparison, periods, the
//! dimension filter, which documents the reader may read, and the codes
//! and names of what rows are about.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{Datelike, Months, NaiveDate};
use erpcraft_kernel::{Ctx, Decimal, Error, JsonSchema, Result, document, entity, schemars};
use serde::Deserialize;
use uuid::Uuid;

/// The range a report compares with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Compare {
    /// The same number of days just before `from`.
    PriorPeriod,
    /// A year earlier.
    PriorYear,
}

/// The periods rows are grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Period {
    #[default]
    Month,
    Quarter,
    Year,
}

impl Period {
    /// The first day of the period `day` is in.
    pub(crate) fn start(self, day: NaiveDate) -> NaiveDate {
        let month = match self {
            Period::Month => day.month(),
            Period::Quarter => (day.month0() / 3) * 3 + 1,
            Period::Year => 1,
        };
        NaiveDate::from_ymd_opt(day.year(), month, 1).unwrap_or(day)
    }
}

/// A year before `day`; the last of a month stays the last of its month,
/// so 28 February 2025 is 29 February 2024.
pub(crate) fn year_before(day: NaiveDate) -> Option<NaiveDate> {
    let after = day.succ_opt()?;
    if after.day() == 1 {
        after.checked_sub_months(Months::new(12))?.pred_opt()
    } else {
        day.checked_sub_months(Months::new(12))
    }
}

/// The range a report compares `from..=to` with, and how many days a date
/// of it is moved forward to be matched with the report's own.
pub(crate) fn compared(
    from: NaiveDate,
    to: NaiveDate,
    compare: Compare,
) -> Result<(NaiveDate, NaiveDate)> {
    let range = match compare {
        Compare::PriorPeriod => {
            let days = (to - from).num_days() + 1;
            let end = from.pred_opt();
            end.and_then(|end| {
                end.checked_sub_days(chrono::Days::new(days as u64 - 1))
                    .map(|start| (start, end))
            })
        }
        Compare::PriorYear => from
            .checked_sub_months(Months::new(12))
            .zip(year_before(to)),
    };
    range.ok_or_else(|| Error::validation("the range to compare with is outside the calendar"))
}

/// A date of the compared range as the date of the report's range it
/// stands beside.
pub(crate) fn moved_forward(
    day: NaiveDate,
    from: NaiveDate,
    to: NaiveDate,
    compare: Compare,
) -> NaiveDate {
    match compare {
        Compare::PriorPeriod => {
            let days = (to - from).num_days() + 1;
            day.checked_add_days(chrono::Days::new(days as u64))
                .unwrap_or(day)
        }
        Compare::PriorYear => day.checked_add_months(Months::new(12)).unwrap_or(day),
    }
}

/// Refuse a range that runs backwards.
pub(crate) fn check_range(from: NaiveDate, to: NaiveDate) -> Result<()> {
    if from > to {
        return Err(Error::validation("`from` cannot be after `to`").with_field("from"));
    }
    Ok(())
}

/// `part` in percent of `whole`, to two places; none when `whole` is 0.
pub(crate) fn percent(part: Decimal, whole: Decimal) -> Option<Decimal> {
    (!whole.is_zero()).then(|| (part * Decimal::ONE_HUNDRED / whole).round_dp(2))
}

/// `a` over `b`, to four places; none when `b` is 0.
pub(crate) fn ratio(a: Decimal, b: Decimal) -> Option<Decimal> {
    (!b.is_zero()).then(|| (a / b).round_dp(4))
}

/// Of `ids`, the documents the reader may read: every one for an
/// administrator, else as `document::readable` decides.
pub(crate) async fn readable_documents(
    ctx: &mut Ctx<'_>,
    ids: BTreeSet<Uuid>,
) -> Result<BTreeSet<Uuid>> {
    if ctx.actor.is_admin || ids.is_empty() {
        return Ok(ids);
    }
    let ids: Vec<Uuid> = ids.into_iter().collect();
    let mut out = BTreeSet::new();
    for chunk in ids.chunks(1000) {
        out.extend(document::readable(ctx, chunk).await?);
    }
    Ok(out)
}

/// The dimension sets that carry, for each dimension among `values`, one
/// of them. Empty `values`: every set, which this answers with `None`.
pub(crate) async fn sets_with(
    ctx: &mut Ctx<'_>,
    values: &[Uuid],
    sets: &BTreeSet<Uuid>,
) -> Result<Option<BTreeSet<Uuid>>> {
    if values.is_empty() {
        return Ok(None);
    }
    let dimensions: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT dimension_id FROM dimension_value WHERE id = ANY ($1)")
            .bind(values)
            .fetch_all(&mut *ctx.conn)
            .await?;
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT set_id, dimension_id FROM dimension_set_value
         WHERE set_id = ANY ($1) AND value_id = ANY ($2)",
    )
    .bind(sets.iter().copied().collect::<Vec<_>>())
    .bind(values)
    .fetch_all(&mut *ctx.conn)
    .await?;
    let mut found: HashMap<Uuid, BTreeSet<Uuid>> = HashMap::new();
    for (set, dimension) in rows {
        found.entry(set).or_default().insert(dimension);
    }
    Ok(Some(
        found
            .into_iter()
            .filter(|(_, d)| d.len() == dimensions.len())
            .map(|(set, _)| set)
            .collect(),
    ))
}

/// The value of `dimension` each of `sets` carries.
pub(crate) async fn values_of(
    ctx: &mut Ctx<'_>,
    dimension: Uuid,
    sets: &BTreeSet<Uuid>,
) -> Result<HashMap<Uuid, Uuid>> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT set_id, value_id FROM dimension_set_value
         WHERE dimension_id = $1 AND set_id = ANY ($2)",
    )
    .bind(dimension)
    .bind(sets.iter().copied().collect::<Vec<_>>())
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// The item group (a price group of kind item) of each of `items`.
pub(crate) async fn groups_of(
    ctx: &mut Ctx<'_>,
    items: &BTreeSet<Uuid>,
) -> Result<HashMap<Uuid, Uuid>> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT member_id, group_id FROM price_group_member
         WHERE kind = 'item' AND member_id = ANY ($1)",
    )
    .bind(items.iter().copied().collect::<Vec<_>>())
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// The code and name of records of `entity` (in `table`), for those the
/// reader may read: a row about another shows its id alone.
pub(crate) async fn labels(
    ctx: &mut Ctx<'_>,
    entity_name: &str,
    table: &str,
    ids: &BTreeSet<Uuid>,
) -> Result<BTreeMap<Uuid, (String, String)>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let ids: Vec<Uuid> = ids.iter().copied().collect();
    let readable = entity::readable_ids(ctx, entity_name, &ids).await?;
    let readable: Vec<Uuid> = readable.into_iter().collect();
    let (code, name) = (
        !ctx.actor
            .may_withhold(&format!("{entity_name}:read"), "code"),
        !ctx.actor
            .may_withhold(&format!("{entity_name}:read"), "name"),
    );
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(&format!(
        "SELECT id, code, name FROM {table} WHERE id = ANY ($1)"
    ))
    .bind(&readable)
    .fetch_all(&mut *ctx.conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, c, n)| {
            (
                id,
                (
                    if code { c } else { String::new() },
                    if name { n } else { String::new() },
                ),
            )
        })
        .collect())
}

/// The code and name of `id` in `labels`, as a row shows them.
pub(crate) fn label(
    labels: &BTreeMap<Uuid, (String, String)>,
    id: Option<Uuid>,
) -> (Option<String>, Option<String>) {
    match id.and_then(|id| labels.get(&id)) {
        Some((code, name)) => (
            Some(code.clone()).filter(|c| !c.is_empty()),
            Some(name.clone()).filter(|n| !n.is_empty()),
        ),
        None => (None, None),
    }
}

/// The item group (a price group of kind item) of each of `items`, for
/// other modules.
pub async fn groups_of_items(
    ctx: &mut Ctx<'_>,
    items: &BTreeSet<Uuid>,
) -> Result<HashMap<Uuid, Uuid>> {
    groups_of(ctx, items).await
}
