//! Payment terms: when an item is due, and the discount for paying early.

use chrono::{Datelike, Days, NaiveDate};
use erpcraft_kernel::{
    Decimal, EntityMeta, Error, FieldMeta, FieldType, PgConnection, Result, round_money,
};
use uuid::Uuid;

pub(crate) fn step_entity() -> EntityMeta {
    EntityMeta::new("payment_terms_step", "Payment terms discount step")
        .field(
            FieldMeta::new(
                "terms_id",
                "Payment terms",
                FieldType::link("payment_terms"),
            )
            .required()
            .immutable(),
        )
        .field(
            FieldMeta::new("discount_percent", "Discount", FieldType::Decimal)
                .percent(6)
                .required(),
        )
        .field(
            FieldMeta::new("discount_days", "Within days", FieldType::Int)
                .required()
                .help("Counted as the terms' net days are."),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["terms_id", "discount_days"])
}

pub(crate) fn instalment_entity() -> EntityMeta {
    EntityMeta::new("payment_terms_instalment", "Payment terms instalment")
        .field(
            FieldMeta::new(
                "terms_id",
                "Payment terms",
                FieldType::link("payment_terms"),
            )
            .required()
            .immutable(),
        )
        .field(
            FieldMeta::new("percent", "Part", FieldType::Decimal)
                .percent(6)
                .required()
                .help(
                    "Percent of the invoice due on this day. The parts leave something for \
                     the last, which is what is left, due at the terms' net days.",
                ),
        )
        .field(
            FieldMeta::new("days", "Due after days", FieldType::Int)
                .required()
                .help("Counted as the terms' net days are, and fewer than them."),
        )
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["terms_id", "days"])
}

pub(crate) fn entity() -> EntityMeta {
    EntityMeta::new("payment_terms", "Payment terms")
        .field(FieldMeta::new("code", "Code", FieldType::Text).required())
        .field(FieldMeta::new("name", "Name", FieldType::Text).required())
        .field(FieldMeta::new("net_days", "Net days", FieldType::Int).help(
            "Days after the invoice's date (or after the end of its month) the invoice is \
                 due. 0: due on its date.",
        ))
        .field(
            FieldMeta::new("end_of_month", "From the end of the month", FieldType::Bool).help(
                "Count the days from the last day of the invoice's month rather than from its \
                 date.",
            ),
        )
        .field(
            FieldMeta::new(
                "discount_percent",
                "Early-payment discount",
                FieldType::Decimal,
            )
            .percent(6)
            .help(
                "Percent of what is owed the party may keep back when paying within the \
                     discount days. Give the days with it.",
            ),
        )
        .field(
            FieldMeta::new("discount_days", "Discount days", FieldType::Int)
                .help("Days, counted as the net days are, within which the discount may be taken."),
        )
        .field(FieldMeta::new("active", "Active", FieldType::Bool))
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["code"])
        .search(&["code", "name"])
}

/// Payment terms as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Terms {
    pub id: Uuid,
    pub code: String,
    pub net_days: i32,
    pub end_of_month: bool,
    pub discount_percent: Option<Decimal>,
    pub discount_days: Option<i32>,
    /// Further early-payment discounts after the first: percent and days.
    #[sqlx(skip)]
    pub steps: Vec<(Decimal, i32)>,
    /// The parts due before the last: percent and days. The last part is
    /// what is left, due at the net days.
    #[sqlx(skip)]
    pub instalments: Vec<(Decimal, i32)>,
}

impl Terms {
    pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Terms> {
        let terms: Option<Terms> = sqlx::query_as(
            "SELECT id, code, net_days, end_of_month, discount_percent, discount_days
             FROM payment_terms WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        let mut terms = terms.ok_or_else(|| Error::not_found("payment terms"))?;
        terms.steps = sqlx::query_as(
            "SELECT discount_percent, discount_days FROM payment_terms_step
             WHERE terms_id = $1 ORDER BY discount_days",
        )
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
        terms.instalments = sqlx::query_as(
            "SELECT percent, days FROM payment_terms_instalment
             WHERE terms_id = $1 ORDER BY days",
        )
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
        Ok(terms)
    }

    /// The day counting starts from: the date itself, or the last day of
    /// its month.
    fn start(&self, date: NaiveDate) -> NaiveDate {
        if !self.end_of_month {
            return date;
        }
        let (year, month) = if date.month() == 12 {
            (date.year() + 1, 1)
        } else {
            (date.year(), date.month() + 1)
        };
        NaiveDate::from_ymd_opt(year, month, 1)
            .and_then(|first| first.pred_opt())
            .unwrap_or(date)
    }

    fn after(&self, date: NaiveDate, days: i32) -> NaiveDate {
        self.start(date)
            .checked_add_days(Days::new(days.max(0) as u64))
            .unwrap_or(NaiveDate::MAX)
    }
}

/// When an item dated `date` is due under `terms`, and, if the terms give
/// one, the last day of its early-payment discount and the discount on
/// `owed` (in the item's currency, rounded to `decimals`). No discount is
/// offered on a credit, or one that would round to nothing.
pub fn due_dates(
    terms: &Terms,
    date: NaiveDate,
    owed: Decimal,
    decimals: u32,
) -> (NaiveDate, Option<(NaiveDate, Decimal)>) {
    let due = terms.after(date, terms.net_days);
    let discount = match (terms.discount_percent, terms.discount_days) {
        (Some(percent), Some(days)) if owed > Decimal::ZERO => {
            let amount = round_money(owed * percent / Decimal::ONE_HUNDRED, decimals);
            (amount > Decimal::ZERO && amount < owed).then(|| (terms.after(date, days), amount))
        }
        _ => None,
    };
    (due, discount)
}

/// Every early-payment discount an item dated `date` offers under `terms`,
/// earliest first: the last day of each and the discount on `owed`, in the
/// item's currency rounded to `decimals`. The terms' own discount and their
/// further steps (2 % within 10 days, 1 % within 20). None on a credit,
/// or one that would round to nothing; of two steps ending on one day, the
/// larger.
pub fn discount_steps(
    terms: &Terms,
    date: NaiveDate,
    owed: Decimal,
    decimals: u32,
) -> Vec<(NaiveDate, Decimal)> {
    if owed <= Decimal::ZERO {
        return vec![];
    }
    let mut steps: Vec<(NaiveDate, Decimal)> = vec![];
    let own = terms.discount_percent.zip(terms.discount_days);
    for (percent, days) in own.into_iter().chain(terms.steps.iter().copied()) {
        let amount = round_money(owed * percent / Decimal::ONE_HUNDRED, decimals);
        if amount <= Decimal::ZERO || amount >= owed {
            continue;
        }
        let until = terms.after(date, days);
        match steps.iter_mut().find(|(day, _)| *day == until) {
            Some((_, kept)) => *kept = (*kept).max(amount),
            None => steps.push((until, amount)),
        }
    }
    steps.sort();
    steps
}

/// The parts an item dated `date` is due in under `terms`, earliest
/// first: each one's due date and amount of `owed`, in the item's currency
/// rounded to `decimals`, the last taking what is left at the net due
/// date. Empty when the terms are not in parts, on a credit, or when the
/// parts come to one. Parts that fall on one day are one part; a part that
/// would round to nothing is left to the next.
pub fn instalments(
    terms: &Terms,
    date: NaiveDate,
    owed: Decimal,
    decimals: u32,
) -> Vec<(NaiveDate, Decimal)> {
    if owed <= Decimal::ZERO || terms.instalments.is_empty() {
        return vec![];
    }
    let due = terms.after(date, terms.net_days);
    let mut parts: Vec<(NaiveDate, Decimal)> = vec![];
    let mut left = owed;
    for (percent, days) in &terms.instalments {
        let amount = round_money(owed * percent / Decimal::ONE_HUNDRED, decimals).min(left);
        if amount <= Decimal::ZERO {
            continue;
        }
        let on = terms.after(date, *days).min(due);
        match parts.iter_mut().find(|(day, _)| *day == on) {
            Some((_, kept)) => *kept += amount,
            None => parts.push((on, amount)),
        }
        left -= amount;
    }
    if left > Decimal::ZERO {
        match parts.iter_mut().find(|(day, _)| *day == due) {
            Some((_, kept)) => *kept += left,
            None => parts.push((due, left)),
        }
    }
    parts.sort();
    if parts.len() < 2 { vec![] } else { parts }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(net: i32, eom: bool, discount: Option<(&str, i32)>) -> Terms {
        Terms {
            id: Uuid::nil(),
            code: "T".into(),
            net_days: net,
            end_of_month: eom,
            discount_percent: discount.map(|(p, _)| p.parse().unwrap()),
            discount_days: discount.map(|(_, d)| d),
            steps: vec![],
            instalments: vec![],
        }
    }

    fn day(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    #[test]
    fn net_and_end_of_month() {
        let owed: Decimal = "119".parse().unwrap();
        let (due, discount) = due_dates(&terms(30, false, None), day("2026-01-31"), owed, 2);
        assert_eq!(due, day("2026-03-02"));
        assert!(discount.is_none());
        // Thirty days after the end of February.
        let (due, _) = due_dates(&terms(30, true, None), day("2026-02-10"), owed, 2);
        assert_eq!(due, day("2026-03-30"));
        // December rolls into the next year.
        let (due, _) = due_dates(&terms(0, true, None), day("2026-12-05"), owed, 2);
        assert_eq!(due, day("2026-12-31"));
    }

    #[test]
    fn two_ten_net_thirty() {
        let owed: Decimal = "119".parse().unwrap();
        let (due, discount) = due_dates(
            &terms(30, false, Some(("2", 10))),
            day("2026-03-25"),
            owed,
            2,
        );
        assert_eq!(due, day("2026-04-24"));
        assert_eq!(discount, Some((day("2026-04-04"), "2.38".parse().unwrap())));
        // A credit is offered nothing.
        let (_, discount) = due_dates(
            &terms(30, false, Some(("2", 10))),
            day("2026-03-25"),
            -owed,
            2,
        );
        assert!(discount.is_none());
    }

    #[test]
    fn two_ten_one_twenty_net_thirty() {
        let mut t = terms(30, false, Some(("2", 10)));
        t.steps = vec![("1".parse().unwrap(), 20)];
        let owed: Decimal = "119".parse().unwrap();
        let steps = discount_steps(&t, day("2026-03-25"), owed, 2);
        assert_eq!(
            steps,
            [
                (day("2026-04-04"), "2.38".parse().unwrap()),
                (day("2026-04-14"), "1.19".parse().unwrap()),
            ]
        );
        assert!(discount_steps(&t, day("2026-03-25"), -owed, 2).is_empty());
    }

    #[test]
    fn a_third_now_a_third_in_thirty_days_the_rest_in_sixty() {
        let mut t = terms(60, false, None);
        t.instalments = vec![
            ("33.333333".parse().unwrap(), 0),
            ("33.333333".parse().unwrap(), 30),
        ];
        let owed: Decimal = "100".parse().unwrap();
        let parts = instalments(&t, day("2026-03-01"), owed, 2);
        assert_eq!(
            parts,
            [
                (day("2026-03-01"), "33.33".parse().unwrap()),
                (day("2026-03-31"), "33.33".parse().unwrap()),
                (day("2026-04-30"), "33.34".parse().unwrap()),
            ]
        );
        assert_eq!(parts.iter().map(|(_, a)| *a).sum::<Decimal>(), owed);
        // Nothing in parts on a credit, or for terms without parts.
        assert!(instalments(&t, day("2026-03-01"), -owed, 2).is_empty());
        assert!(instalments(&terms(30, false, None), day("2026-03-01"), owed, 2).is_empty());
        // A part too small to be anything is left to the last.
        let tiny = instalments(&t, day("2026-03-01"), "0.01".parse().unwrap(), 2);
        assert!(tiny.is_empty(), "{tiny:?}");
    }
}
