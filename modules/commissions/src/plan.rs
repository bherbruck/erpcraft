//! Which plan a line earns under, and what a basis earns under a plan.

use chrono::NaiveDate;
use erpcraft_kernel::{Decimal, PgConnection, Result};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Plan {
    pub id: Uuid,
    pub basis: String,
    pub trigger: String,
    pub sales_rep_id: Option<Uuid>,
    pub item_group_id: Option<Uuid>,
    pub valid_from: Option<NaiveDate>,
    pub valid_to: Option<NaiveDate>,
    #[sqlx(skip)]
    pub tiers: Vec<(Decimal, Decimal)>,
}

/// The company's active plans, each with its tiers by where they start.
pub(crate) async fn plans(conn: &mut PgConnection, company_id: Uuid) -> Result<Vec<Plan>> {
    let mut plans: Vec<Plan> = sqlx::query_as(
        "SELECT id, basis, trigger, sales_rep_id, item_group_id, valid_from, valid_to
         FROM commission_plan WHERE company_id = $1 AND active ORDER BY code",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    let tiers: Vec<(Uuid, Decimal, Decimal)> = sqlx::query_as(
        "SELECT plan_id, from_amount, percent FROM commission_tier
         WHERE company_id = $1 ORDER BY plan_id, from_amount",
    )
    .bind(company_id)
    .fetch_all(&mut *conn)
    .await?;
    for plan in &mut plans {
        plan.tiers = tiers
            .iter()
            .filter(|t| t.0 == plan.id)
            .map(|t| (t.1, t.2))
            .collect();
    }
    Ok(plans)
}

/// The plan a line of `rep` for an item of `group` earns under on `day`:
/// the most specific one in force, the salesperson's and the group's
/// first.
pub(crate) fn plan_for(
    plans: &[Plan],
    rep: Uuid,
    group: Option<Uuid>,
    day: NaiveDate,
) -> Option<&Plan> {
    plans
        .iter()
        .filter(|p| p.sales_rep_id.is_none_or(|r| r == rep))
        .filter(|p| p.item_group_id.is_none() || p.item_group_id == group)
        .filter(|p| p.valid_from.is_none_or(|f| f <= day) && p.valid_to.is_none_or(|t| t >= day))
        .max_by_key(|p| {
            (
                p.sales_rep_id.is_some() && p.item_group_id.is_some(),
                p.sales_rep_id.is_some(),
                p.item_group_id.is_some(),
            )
        })
}

/// What `basis` earns under tiers that start at their amounts: each band
/// at its own percent. Below the first tier nothing; a negative basis (more
/// taken back than earned) at the first tier's percent.
pub(crate) fn earned(tiers: &[(Decimal, Decimal)], basis: Decimal) -> Decimal {
    let hundred = Decimal::ONE_HUNDRED;
    if basis < Decimal::ZERO {
        return tiers
            .first()
            .map_or(Decimal::ZERO, |t| basis * t.1 / hundred);
    }
    let mut total = Decimal::ZERO;
    for (i, (from, percent)) in tiers.iter().enumerate() {
        let to = tiers.get(i + 1).map(|t| t.0);
        if basis <= *from {
            break;
        }
        let top = to.map_or(basis, |to| basis.min(to));
        total += (top - from) * percent / hundred;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: i64) -> Decimal {
        Decimal::from(n)
    }

    #[test]
    fn bands_earn_at_their_own_rates() {
        let tiers = [(d(10_000), d(5)), (d(50_000), d(7))];
        assert_eq!(earned(&tiers, d(60_000)), d(2_700));
        assert_eq!(earned(&tiers, d(8_000)), d(0));
        assert_eq!(earned(&tiers, d(30_000)), d(1_000));
        let flat = [(d(0), d(3))];
        assert_eq!(earned(&flat, d(1_000)), d(30));
        assert_eq!(earned(&flat, d(-500)), d(-15));
        assert_eq!(earned(&[], d(1_000)), d(0));
    }

    #[test]
    fn the_most_specific_plan_wins() {
        let rep = Uuid::new_v4();
        let group = Uuid::new_v4();
        let plan = |r: Option<Uuid>, g: Option<Uuid>| Plan {
            id: Uuid::new_v4(),
            basis: "net".into(),
            trigger: "invoice".into(),
            sales_rep_id: r,
            item_group_id: g,
            valid_from: None,
            valid_to: None,
            tiers: vec![],
        };
        let plans = vec![
            plan(None, None),
            plan(Some(rep), None),
            plan(None, Some(group)),
        ];
        let day = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        assert_eq!(
            plan_for(&plans, rep, Some(group), day).unwrap().id,
            plans[1].id
        );
        assert_eq!(
            plan_for(&plans, Uuid::new_v4(), Some(group), day)
                .unwrap()
                .id,
            plans[2].id
        );
        assert_eq!(
            plan_for(&plans, Uuid::new_v4(), None, day).unwrap().id,
            plans[0].id
        );
    }
}
