//! Review 9: budgets and the cash flow statement.
#![allow(dead_code)]

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{TestApp, dec};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

struct Books {
    company: String,
    account: BTreeMap<String, String>,
    bank: String,
}

impl Books {
    async fn new(t: &TestApp, first_year: i32, years: i32) -> Books {
        let created = run(
            t,
            "company.setup",
            json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                    "first_year": first_year, "years": years }),
        )
        .await;
        let company = created["company"]["id"].as_str().unwrap().to_owned();
        let (_, list) = t
            .get(
                &t.admin,
                &format!("/api/entities/account?company_id={company}&limit=500"),
            )
            .await;
        Books {
            account: list["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| {
                    (
                        a["code"].as_str().unwrap().to_owned(),
                        a["id"].as_str().unwrap().to_owned(),
                    )
                })
                .collect(),
            bank: created["roles"]["bank"]["id"].as_str().unwrap().to_owned(),
            company,
        }
    }

    fn id(&self, code: &str) -> String {
        if code == "BANK" {
            return self.bank.clone();
        }
        self.account[code].clone()
    }

    async fn journal(&self, t: &TestApp, date: &str, debit: &str, credit: &str, amount: &str) {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/journal_entry",
                json!({ "company_id": self.company, "doc_date": date,
                        "body": { "lines": [
                            { "account_id": self.id(debit), "debit": amount },
                            { "account_id": self.id(credit), "credit": amount }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "journal_entry.post", json!({ "id": draft["id"] })).await;
    }
}

/// A fiscal year March to February; the comparative year a year earlier
/// runs 2023-03-01 to 2024-02-29. A sale on 2024-02-29 belongs to it.
#[tokio::test]
async fn the_comparative_year_keeps_the_29th_of_february() {
    let t = TestApp::start().await;
    let b = Books::new(&t, 2023, 3).await;
    b.journal(&t, "2024-02-29", "BANK", "4010", "700.00").await;
    let flow = run(
        &t,
        "gl.cash_flow",
        json!({ "company_id": b.company, "from": "2024-03-01", "to": "2025-02-28",
                "compare": true }),
    )
    .await;
    let earlier = &flow["columns"][1];
    assert_eq!(earlier["from"], "2023-03-01", "{flow}");
    assert_eq!(
        earlier["to"], "2024-02-29",
        "the comparative year ends a day short: {earlier}"
    );
    assert!(close(dec(&earlier["net_income"]), 700.0), "{earlier}");
    // Per month too: February 2024 has 29 days.
    let monthly = run(
        &t,
        "gl.cash_flow",
        json!({ "company_id": b.company, "from": "2025-02-01", "to": "2025-02-28",
                "by_month": true, "compare": true }),
    )
    .await;
    assert!(
        close(dec(&monthly["columns"][1]["net_income"]), 700.0),
        "February 2024 lost its 29th: {}",
        monthly["columns"][1]
    );
    t.stop().await;
}

/// A bank or cash flag on an expense account (petty cash expensed at
/// once, a mistake) is allowed. The statement then counts it as cash at
/// the start but not in the period, so one month's end is not the next
/// month's start.
#[tokio::test]
async fn one_months_closing_cash_is_the_next_months_opening_cash() {
    let t = TestApp::start().await;
    let b = Books::new(&t, 2026, 1).await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{}", b.id("6070")),
            json!({ "is_bank": true }),
        )
        .await;
    if status != StatusCode::OK {
        // Refusing a bank flag on an expense account is a fine answer.
        eprintln!("refused: {body}");
        t.stop().await;
        return;
    }
    b.journal(&t, "2026-01-15", "BANK", "3010", "5000.00").await;
    b.journal(&t, "2026-01-20", "6070", "BANK", "400.00").await;
    let monthly = run(
        &t,
        "gl.cash_flow",
        json!({ "company_id": b.company, "from": "2026-01-01", "to": "2026-02-28",
                "by_month": true }),
    )
    .await;
    let cols = monthly["columns"].as_array().unwrap();
    assert!(
        close(dec(&cols[0]["cash_at_end"]), dec(&cols[1]["cash_at_start"])),
        "January ends with {} in cash, February starts with {}: {monthly}",
        cols[0]["cash_at_end"],
        cols[1]["cash_at_start"]
    );
    t.stop().await;
}

/// Someone allowed to read the budget but not its lines (or kept from
/// them) still sees every budgeted amount through budget.report.
#[tokio::test]
async fn the_budget_report_keeps_to_the_rules_on_budget_lines() {
    let t = TestApp::start().await;
    let b = Books::new(&t, 2026, 1).await;
    let budget = t
        .create(
            "budget",
            json!({ "company_id": b.company, "code": "B2026", "name": "Budget 2026",
                    "starts_on": "2026-01-01", "ends_on": "2026-12-31" }),
        )
        .await;
    run(
        &t,
        "budget.import",
        json!({ "id": budget, "csv": "account,month,amount\n6070,2026-01,98765\n" }),
    )
    .await;
    let (token, _) = t
        .user_with_rules(
            "viewer@example.com",
            json!(["budget:read", "gl_entry:read", { "deny": "budget_line:read" }]),
            Some(&b.company),
        )
        .await;
    let (status, lines) = t
        .get(
            &token,
            &format!("/api/entities/budget_line?budget_id={budget}"),
        )
        .await;
    assert!(
        status != StatusCode::OK || !lines.to_string().contains("98765"),
        "the lines are withheld from this user: {lines}"
    );
    let (status, body) = t
        .command(&token, "budget.report", json!({ "id": budget }))
        .await;
    assert!(
        status != StatusCode::OK || !body.to_string().contains("98765"),
        "budget.report shows lines the user may not read: {body}"
    );
    t.stop().await;
}
