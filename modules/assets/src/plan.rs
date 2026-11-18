//! The depreciation of one asset, month by month: a pure calculation, so
//! that a run, a disposal and the schedule report all come to the same
//! cents.

use chrono::{Datelike, Duration, NaiveDate};
use erpcraft_kernel::{Decimal, round_money};

/// How an asset loses value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The same amount every month, the last month taking what rounding
    /// left.
    StraightLine,
    /// A yearly rate of the book value at the start of each calendar year,
    /// switching to straight line over the remaining life once that is
    /// more.
    DecliningBalance,
    /// Not depreciated: land.
    None,
}

impl Method {
    pub fn parse(text: &str) -> Method {
        match text {
            "straight_line" => Method::StraightLine,
            "declining_balance" => Method::DecliningBalance,
            _ => Method::None,
        }
    }
}

/// What a plan is worked out from.
#[derive(Debug, Clone)]
pub struct Params {
    pub cost: Decimal,
    pub salvage: Decimal,
    pub method: Method,
    /// Useful life in months.
    pub life: i32,
    /// The yearly rate of the declining balance as a fraction: 0.25.
    pub rate: Decimal,
    /// The first month planned: the first month of depreciation, or for an
    /// asset taken over from another system, the month after the one its
    /// opening accumulated depreciation reaches.
    pub basis: NaiveDate,
    /// Months of the life used before `basis`.
    pub elapsed: i32,
    /// Depreciation before `basis`.
    pub accumulated: Decimal,
    /// Decimal places of the accounting currency.
    pub places: u32,
}

/// The first day of the month of `date`.
pub fn first_of(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

/// The first day of the month after the one `month` is in.
pub fn next_month(month: NaiveDate) -> NaiveDate {
    let (year, month) = if month.month() == 12 {
        (month.year() + 1, 1)
    } else {
        (month.year(), month.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(NaiveDate::MAX)
}

/// The last day of the month `date` is in.
pub fn month_end(date: NaiveDate) -> NaiveDate {
    next_month(first_of(date)) - Duration::days(1)
}

/// Whole months from the month of `from` to the month of `to`: 0 for the
/// same month, negative when `to` is earlier.
pub fn months_between(from: NaiveDate, to: NaiveDate) -> i32 {
    (to.year() - from.year()) * 12 + to.month() as i32 - from.month() as i32
}

/// Every month's depreciation from `basis` to the end of the life, as the
/// first day of the month and the amount. Months of nothing are left out.
pub fn plan(p: &Params) -> Vec<(NaiveDate, Decimal)> {
    let mut out = Vec::new();
    if p.method == Method::None || p.life <= 0 || p.elapsed >= p.life {
        return out;
    }
    let round = |d: Decimal| round_money(d, p.places);
    let remaining = Decimal::from(p.life - p.elapsed);
    let straight = round((p.cost - p.salvage - p.accumulated) / remaining);
    let mut accumulated = p.accumulated;
    let mut elapsed = p.elapsed;
    let mut month = p.basis;
    // The declining balance is worked out per calendar year: how much the
    // block of months to December comes to, and how much a month is.
    let (mut block_left, mut block_total, mut block_done, mut block_month) =
        (0i32, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
    while elapsed < p.life {
        let left = p.cost - p.salvage - accumulated;
        if left <= Decimal::ZERO {
            break;
        }
        let last = elapsed + 1 == p.life;
        let amount = match p.method {
            Method::StraightLine => {
                if last {
                    left
                } else {
                    straight.min(left)
                }
            }
            Method::DecliningBalance => {
                if block_left == 0 {
                    let to_december = 13 - month.month() as i32;
                    let months = to_december.min(p.life - elapsed);
                    let book = p.cost - accumulated;
                    let declining = book * p.rate;
                    let straight = left * Decimal::from(12) / Decimal::from(p.life - elapsed);
                    let yearly = declining.max(straight);
                    block_total = if elapsed + months == p.life {
                        left
                    } else {
                        round(yearly * Decimal::from(months) / Decimal::from(12)).min(left)
                    };
                    block_month = round(yearly / Decimal::from(12));
                    block_left = months;
                    block_done = Decimal::ZERO;
                }
                let amount = if block_left == 1 {
                    block_total - block_done
                } else {
                    block_month.min(block_total - block_done)
                };
                block_left -= 1;
                block_done += amount;
                amount
            }
            Method::None => Decimal::ZERO,
        };
        if amount > Decimal::ZERO {
            out.push((month, amount));
        }
        accumulated += amount;
        elapsed += 1;
        month = next_month(month);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn params(method: Method, cost: &str, life: i32, rate: &str, basis: &str) -> Params {
        Params {
            cost: d(cost),
            salvage: Decimal::ZERO,
            method,
            life,
            rate: d(rate),
            basis: date(basis),
            elapsed: 0,
            accumulated: Decimal::ZERO,
            places: 2,
        }
    }

    #[test]
    fn straight_line_takes_the_remainder_in_the_last_month() {
        let plan = plan(&params(Method::StraightLine, "1000", 12, "0", "2026-01-01"));
        assert_eq!(plan.len(), 12);
        assert!(plan[..11].iter().all(|(_, a)| *a == d("83.33")));
        assert_eq!(plan[11], (date("2026-12-01"), d("83.37")));
        let total: Decimal = plan.iter().map(|(_, a)| *a).sum();
        assert_eq!(total, d("1000"));
    }

    #[test]
    fn straight_line_stops_at_the_salvage_value() {
        let mut p = params(Method::StraightLine, "1200", 10, "0", "2026-01-01");
        p.salvage = d("200");
        let plan = plan(&p);
        assert_eq!(plan.len(), 10);
        assert!(plan.iter().all(|(_, a)| *a == d("100")));
    }

    #[test]
    fn declining_balance_per_calendar_year() {
        // 25 % of 10,000 in the first year: 2,500, December the remainder.
        let plan = plan(&params(
            Method::DecliningBalance,
            "10000",
            120,
            "0.25",
            "2026-01-01",
        ));
        let year = |y: i32| -> Decimal {
            plan.iter()
                .filter(|(m, _)| m.year() == y)
                .map(|(_, a)| *a)
                .sum()
        };
        assert_eq!(plan[0].1, d("208.33"));
        assert_eq!(plan[11].1, d("208.37"));
        assert_eq!(year(2026), d("2500"));
        // 25 % of the 7,500 left.
        assert_eq!(year(2027), d("1875"));
        let total: Decimal = plan.iter().map(|(_, a)| *a).sum();
        assert_eq!(total, d("10000"));
        assert_eq!(plan.last().unwrap().0, date("2035-12-01"));
    }

    #[test]
    fn declining_balance_switches_to_straight_line_when_that_is_more() {
        // Five years at 25 %: 2,500 then 1,875; in the third year straight
        // line over the three years left is 1,875 too, and from the fourth
        // it is more than 25 % of the book value.
        let plan = plan(&params(
            Method::DecliningBalance,
            "10000",
            60,
            "0.25",
            "2026-01-01",
        ));
        let year = |y: i32| -> Decimal {
            plan.iter()
                .filter(|(m, _)| m.year() == y)
                .map(|(_, a)| *a)
                .sum()
        };
        assert_eq!(year(2026), d("2500"));
        assert_eq!(year(2027), d("1875"));
        assert_eq!(year(2028), d("1875"));
        assert_eq!(year(2029), d("1875"));
        assert_eq!(year(2030), d("1875"));
        assert_eq!(plan.len(), 60);
    }

    #[test]
    fn a_part_year_is_pro_rata() {
        // In service from April: nine months of the year.
        let plan = plan(&params(
            Method::DecliningBalance,
            "10000",
            120,
            "0.25",
            "2026-04-01",
        ));
        let first: Decimal = plan[..9].iter().map(|(_, a)| *a).sum();
        assert_eq!(first, d("1875"));
        assert_eq!(plan[8], (date("2026-12-01"), d("208.36")));
    }

    #[test]
    fn a_taken_over_asset_spreads_what_is_left_over_the_life_left() {
        let mut p = params(Method::StraightLine, "3600", 36, "0", "2026-01-01");
        p.elapsed = 24;
        p.accumulated = d("2400");
        let plan = plan(&p);
        assert_eq!(plan.len(), 12);
        assert!(plan.iter().all(|(_, a)| *a == d("100")));
    }

    #[test]
    fn land_is_not_depreciated() {
        assert!(plan(&params(Method::None, "50000", 0, "0", "2026-01-01")).is_empty());
    }

    #[test]
    fn months() {
        assert_eq!(month_end(date("2026-02-10")), date("2026-02-28"));
        assert_eq!(next_month(date("2026-12-01")), date("2027-01-01"));
        assert_eq!(months_between(date("2025-11-30"), date("2026-02-01")), 3);
    }
}
