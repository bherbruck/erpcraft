//! Checking the books after a restore (or at any time): every ledger
//! against the others, per company, read with SQL from the tables, as the
//! month-end scenario test checks them. Nothing is changed.
//!
//! - the general ledger balances: debits equal credits, and every posting
//!   balances on its own;
//! - the stock ledger equals its own entries (levels and lot levels are the
//!   sums of what was recorded), and the inventory accounts (the account
//!   rules for `inventory` and `inventory_in_transit`) carry exactly the
//!   value of the stock;
//! - open items: each item is settled by exactly its settlements, and every
//!   account that carries open items has the balance they are carried at
//!   (receivables as debits, payables as credits, with revaluations
//!   standing);
//! - tax: on every tax account, what the postings that recorded tax put
//!   there is exactly the tax recorded (the tax report). Payments to the
//!   tax authority, which record no tax, are left out.

use erpcraft_kernel::{Decimal, PgPool, Result};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// What was checked, such as `trial_balance`.
    pub name: &'static str,
    pub ok: bool,
    /// What it should be and what it is, where a check compares amounts.
    pub expected: Option<Decimal>,
    pub actual: Option<Decimal>,
    /// What does not agree, in words.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompanyReport {
    pub company_id: Uuid,
    pub code: String,
    pub name: String,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ok: bool,
    pub companies: Vec<CompanyReport>,
}

fn check(name: &'static str, expected: Decimal, actual: Decimal, what: &str) -> Check {
    let ok = expected == actual;
    Check {
        name,
        ok,
        expected: Some(expected),
        actual: Some(actual),
        detail: if ok {
            what.to_owned()
        } else {
            format!("{what}: expected {expected}, found {actual}")
        },
    }
}

fn count(name: &'static str, found: i64, what: &str) -> Check {
    Check {
        name,
        ok: found == 0,
        expected: None,
        actual: None,
        detail: if found == 0 {
            format!("no {what}")
        } else {
            format!("{found} {what}")
        },
    }
}

/// Every check, for every company (or the one whose code is given).
pub async fn verify(pool: &PgPool, company_code: Option<&str>) -> Result<Report> {
    let mut conn = pool.acquire().await?;
    // One snapshot for every query, so the ledgers are compared at one
    // moment even while the server runs.
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *conn)
        .await?;
    let companies: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, code, name FROM company WHERE $1::text IS NULL OR code = $1 ORDER BY code",
    )
    .bind(company_code)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = vec![];
    for (id, code, name) in companies {
        let mut checks = vec![];
        let c = &mut *conn;

        // The general ledger.
        let (debit, credit): (Decimal, Decimal) = sqlx::query_as(
            "SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0) FROM gl_entry
             WHERE company_id = $1",
        )
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        checks.push(check(
            "trial_balance",
            debit,
            credit,
            "debits equal credits",
        ));
        let unbalanced: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM (SELECT posting_id FROM gl_entry WHERE company_id = $1
                                   GROUP BY posting_id HAVING sum(debit) <> sum(credit)) u",
        )
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        checks.push(count(
            "postings_balance",
            unbalanced,
            "postings whose debits and credits differ",
        ));

        // The stock ledger against itself.
        let levels: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM stock_level l
             LEFT JOIN (SELECT item_id, warehouse_id, sum(qty) AS qty, sum(value) AS value
                        FROM stock_entry WHERE company_id = $1
                        GROUP BY item_id, warehouse_id) e
                 ON e.item_id = l.item_id AND e.warehouse_id = l.warehouse_id
             WHERE l.company_id = $1
               AND (l.qty <> COALESCE(e.qty, 0) OR l.value <> COALESCE(e.value, 0))",
        )
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        checks.push(count(
            "stock_levels",
            levels,
            "stock levels that are not the sum of their entries",
        ));
        let lots: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM stock_lot_level v
             LEFT JOIN (SELECT el.lot_id, e.warehouse_id, sum(el.qty) AS qty
                        FROM stock_entry_lot el JOIN stock_entry e ON e.id = el.entry_id
                        WHERE e.company_id = $1 GROUP BY el.lot_id, e.warehouse_id) s
                 ON s.lot_id = v.lot_id AND s.warehouse_id = v.warehouse_id
             WHERE v.company_id = $1 AND v.qty <> COALESCE(s.qty, 0)",
        )
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        checks.push(count(
            "lot_levels",
            lots,
            "lot levels that are not the sum of their entries",
        ));

        // Inventory accounts carry the stock's value.
        let (stock_value, inventory): (Decimal, Decimal) = sqlx::query_as(
            "SELECT
                (SELECT COALESCE(sum(value), 0) FROM stock_level WHERE company_id = $1),
                (SELECT COALESCE(sum(g.debit - g.credit), 0) FROM gl_entry g
                 WHERE g.company_id = $1 AND g.account_id IN (
                     SELECT account_id FROM account_rule
                     WHERE company_id = $1
                       AND purpose IN ('inventory', 'inventory_in_transit')))",
        )
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        checks.push(check(
            "inventory",
            stock_value,
            inventory,
            "the inventory accounts carry the value of the stock",
        ));

        // Open items against their settlements, and against their accounts.
        let settled: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM open_item i
             WHERE i.company_id = $1 AND i.settled <> COALESCE(
                 (SELECT sum(s.amount) FROM settlement s WHERE s.open_item_id = i.id), 0)",
        )
        .bind(id)
        .fetch_one(&mut *c)
        .await?;
        checks.push(count(
            "open_item_settlements",
            settled,
            "open items not settled by exactly their settlements",
        ));
        let accounts: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
            "WITH carried AS (
                 SELECT i.account_id,
                        sum(CASE WHEN i.kind = 'receivable' THEN 1 ELSE -1 END
                            * (i.amount - i.settled + COALESCE(
                                (SELECT sum(v.amount) FROM open_item_revaluation v
                                 WHERE v.open_item_id = i.id), 0))) AS carried
                 FROM open_item i WHERE i.company_id = $1 GROUP BY i.account_id
             )
             SELECT a.code, c.carried,
                    COALESCE((SELECT sum(g.debit - g.credit) FROM gl_entry g
                              WHERE g.account_id = c.account_id), 0)
             FROM carried c JOIN account a ON a.id = c.account_id ORDER BY a.code",
        )
        .bind(id)
        .fetch_all(&mut *c)
        .await?;
        let off: Vec<String> = accounts
            .iter()
            .filter(|(_, open, ledger)| open != ledger)
            .map(|(code, open, ledger)| format!("{code}: open items {open}, ledger {ledger}"))
            .collect();
        let (open, ledger) = accounts
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(o, l), a| {
                (o + a.1, l + a.2)
            });
        checks.push(Check {
            name: "open_items",
            ok: off.is_empty(),
            expected: Some(open),
            actual: Some(ledger),
            detail: if off.is_empty() {
                format!(
                    "{} receivable and payable account(s) carry their open items",
                    accounts.len()
                )
            } else {
                off.join("; ")
            },
        });

        // Tax: the tax report equals what tax postings put on tax accounts.
        let tax: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
            "WITH recorded AS (
                 SELECT account_id,
                        sum(CASE WHEN direction = 'paid' THEN amount ELSE -amount END) AS tax
                 FROM tax_entry WHERE company_id = $1 AND account_id IS NOT NULL
                 GROUP BY account_id
             ), taxed AS (
                 SELECT DISTINCT posting_id FROM tax_entry WHERE company_id = $1
             )
             SELECT a.code, r.tax,
                    COALESCE((SELECT sum(g.debit - g.credit) FROM gl_entry g
                              JOIN taxed t ON t.posting_id = g.posting_id
                              WHERE g.account_id = r.account_id), 0)
             FROM recorded r JOIN account a ON a.id = r.account_id ORDER BY a.code",
        )
        .bind(id)
        .fetch_all(&mut *c)
        .await?;
        let off: Vec<String> = tax
            .iter()
            .filter(|(_, recorded, ledger)| recorded != ledger)
            .map(|(code, recorded, ledger)| {
                format!("{code}: tax report {recorded}, ledger {ledger}")
            })
            .collect();
        let (recorded, ledger) = tax
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(r, l), a| {
                (r + a.1, l + a.2)
            });
        checks.push(Check {
            name: "tax",
            ok: off.is_empty(),
            expected: Some(recorded),
            actual: Some(ledger),
            detail: if off.is_empty() {
                format!("{} tax account(s) carry the tax recorded", tax.len())
            } else {
                off.join("; ")
            },
        });

        out.push(CompanyReport {
            company_id: id,
            code,
            name,
            checks,
        });
    }
    sqlx::query("COMMIT").execute(&mut *conn).await?;
    Ok(Report {
        ok: out.iter().all(|c| c.checks.iter().all(|k| k.ok)),
        companies: out,
    })
}

impl Report {
    /// The report as lines of text, one per check.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for company in &self.companies {
            out.push_str(&format!("{} {}\n", company.code, company.name));
            for check in &company.checks {
                out.push_str(&format!(
                    "  {} {:<22} {}\n",
                    if check.ok { "ok  " } else { "FAIL" },
                    check.name,
                    check.detail
                ));
            }
        }
        if self.companies.is_empty() {
            out.push_str("no companies\n");
        }
        out.push_str(if self.ok {
            "the books agree\n"
        } else {
            "the books do NOT agree\n"
        });
        out
    }
}
