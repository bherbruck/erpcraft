//! Budgets by account, month and site, imported and copied from last
//! year's actuals, reported against the ledger; and the cash flow
//! statement by the indirect method, which reconciles to the change in
//! cash, per month and against the year before.

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
    async fn new(t: &TestApp) -> Books {
        let created = run(
            t,
            "company.setup",
            json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                    "first_year": 2025, "years": 2 }),
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

    /// Post a journal entry of one debit and one credit, the debit with
    /// the dimension value given.
    async fn journal(
        &self,
        t: &TestApp,
        date: &str,
        debit: &str,
        credit: &str,
        amount: &str,
        value: Option<&str>,
    ) {
        let mut first = json!({ "account_id": self.id(debit), "debit": amount });
        let mut second = json!({ "account_id": self.id(credit), "credit": amount });
        if let Some(v) = value {
            first["dimension_value_ids"] = json!([v]);
            second["dimension_value_ids"] = json!([v]);
        }
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/journal_entry",
                json!({ "company_id": self.company, "doc_date": date,
                        "body": { "lines": [first, second] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "journal_entry.post", json!({ "id": draft["id"] })).await;
    }
}

#[tokio::test]
async fn budgets_are_set_against_the_ledger_and_cash_flow_reconciles() {
    let t = TestApp::start().await;
    let b = Books::new(&t).await;
    let site = t
        .create(
            "dimension",
            json!({ "company_id": b.company, "code": "SITE", "name": "Site" }),
        )
        .await;
    let value = |code: &'static str| {
        let site = site.clone();
        let company = b.company.clone();
        let t = &t;
        async move {
            t.create(
                "dimension_value",
                json!({ "company_id": company, "dimension_id": site, "code": code, "name": code }),
            )
            .await
        }
    };
    let ohio = value("OH").await;
    let texas = value("TX").await;

    // 2025: sales and rent in Ohio and Texas.
    b.journal(&t, "2025-01-15", "1200", "4010", "1000.00", Some(&ohio))
        .await;
    b.journal(&t, "2025-01-20", "1200", "4010", "500.00", Some(&texas))
        .await;
    b.journal(&t, "2025-02-10", "6070", "BANK", "400.00", Some(&ohio))
        .await;
    // 2026.
    b.journal(&t, "2026-01-15", "BANK", "3010", "5000.00", None)
        .await;
    b.journal(&t, "2026-01-20", "1500", "BANK", "2000.00", None)
        .await;
    b.journal(&t, "2026-01-25", "1200", "4010", "1200.00", Some(&ohio))
        .await;
    b.journal(&t, "2026-02-10", "1200", "4010", "1800.00", Some(&ohio))
        .await;
    b.journal(&t, "2026-02-20", "BANK", "1200", "1000.00", None)
        .await;
    b.journal(&t, "2026-02-28", "6030", "1510", "100.00", None)
        .await;
    b.journal(&t, "2026-03-05", "BANK", "2500", "1500.00", None)
        .await;
    b.journal(&t, "2026-03-10", "6070", "BANK", "400.00", Some(&ohio))
        .await;

    // ------------------------------------------------------------ budgets
    let budget = t
        .create(
            "budget",
            json!({ "company_id": b.company, "code": "B2026", "name": "Budget 2026",
                    "starts_on": "2026-01-01", "ends_on": "2026-12-31",
                    "dimension_id": site }),
        )
        .await;
    // Last year's actuals plus 10 %, per site.
    let copied = run(
        &t,
        "budget.copy_actuals",
        json!({ "id": budget, "percent": "10" }),
    )
    .await;
    assert_eq!(copied["lines"], 3, "{copied}");
    let report = run(
        &t,
        "budget.report",
        json!({ "id": budget, "by_month": true }),
    )
    .await;
    let row = |report: &Value, code: &str, month: &str, value: Option<&str>| -> Value {
        report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["code"] == code
                    && r["month"] == month
                    && r["dimension_value_id"].as_str() == value
            })
            .cloned()
            .unwrap_or_else(|| panic!("no row {code} {month} {value:?}: {report}"))
    };
    let jan_ohio = row(&report, "4010", "2026-01-01", Some(&ohio));
    assert!(close(dec(&jan_ohio["budget"]), 1100.0), "{jan_ohio}");
    assert!(close(dec(&jan_ohio["actual"]), 1200.0), "{jan_ohio}");
    assert!(close(dec(&jan_ohio["variance"]), 100.0), "{jan_ohio}");
    assert!(
        close(dec(&jan_ohio["variance_percent"]), 9.09),
        "{jan_ohio}"
    );
    assert_eq!(jan_ohio["favourable"], true);
    // Texas sold nothing in January 2026: all of its budget is missed.
    let jan_texas = row(&report, "4010", "2026-01-01", Some(&texas));
    assert!(close(dec(&jan_texas["variance"]), -550.0), "{jan_texas}");
    assert_eq!(jan_texas["favourable"], false);
    // Rent in February was budgeted and not spent: favourable.
    let feb_rent = row(&report, "6070", "2026-02-01", Some(&ohio));
    assert_eq!(feb_rent["favourable"], true, "{feb_rent}");

    // An import of a forecast, wide, by site.
    let forecast = t
        .create(
            "budget",
            json!({ "company_id": b.company, "code": "F2026", "name": "Forecast 2026",
                    "starts_on": "2026-01-01", "ends_on": "2026-12-31", "version": "forecast",
                    "dimension_id": site }),
        )
        .await;
    let imported = run(
        &t,
        "budget.import",
        json!({ "id": forecast, "csv":
            "account,dimension_value,2026-01,2026-02,2026-03\n4010,OH,1200,1500,1500\n6070,OH,400,400,400\n" }),
    )
    .await;
    assert_eq!(imported["lines"], 6, "{imported}");
    // Long, with a mistake: nothing is written.
    let (status, body) = t
        .command(
            &t.admin,
            "budget.import",
            json!({ "id": forecast, "csv": "account,month,amount\n6070,2026-04,400\n9999,2026-04,1\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("line 3"), "{body}");
    let report = run(
        &t,
        "budget.report",
        json!({ "id": forecast, "to": "2026-03-31" }),
    )
    .await;
    let totals = &report["totals"][0];
    // Income 4200 budgeted, 3000 posted in Ohio; rent 1200 against 400.
    assert!(close(dec(&totals["income"]["budget"]), 4200.0), "{report}");
    assert!(close(dec(&totals["income"]["actual"]), 3000.0), "{report}");
    assert!(close(dec(&totals["expense"]["budget"]), 1200.0), "{report}");
    assert!(close(dec(&totals["result"]["budget"]), 3000.0), "{report}");
    // Only one site.
    let texas_only = run(
        &t,
        "budget.report",
        json!({ "id": forecast, "to": "2026-03-31", "dimension_value_id": texas }),
    )
    .await;
    assert!(
        close(dec(&texas_only["totals"][0]["income"]["budget"]), 0.0),
        "{texas_only}"
    );

    // A locked budget does not change.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/budget/{forecast}"),
            json!({ "locked": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = t
        .command(
            &t.admin,
            "budget.import",
            json!({ "id": forecast, "csv": "account,month,amount\n6070,2026-04,400\n" }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/budget_line",
            json!({ "budget_id": forecast, "account_id": b.id("6070"), "month": "2026-05-01",
                    "amount": "1" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // ---------------------------------------------------------- cash flow
    // The loan is financing; the equipment account is investing.
    for (code, flow) in [("2500", "financing"), ("1500", "investing")] {
        let (status, body) = t
            .patch(
                &t.admin,
                &format!("/api/entities/account/{}", b.id(code)),
                json!({ "cash_flow": flow }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    // An income account has no flow of its own.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{}", b.id("4010")),
            json!({ "cash_flow": "operating" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let flow = run(
        &t,
        "gl.cash_flow",
        json!({ "company_id": b.company, "from": "2026-01-01", "to": "2026-03-31",
                "compare": true }),
    )
    .await;
    let q1 = &flow["columns"][0];
    // Net income: 3000 sold, 100 depreciation, 400 rent.
    assert!(close(dec(&q1["net_income"]), 2500.0), "{flow}");
    // Less 2000 more receivables, plus 100 depreciation.
    assert!(close(dec(&q1["operating"]), 600.0), "{flow}");
    assert!(close(dec(&q1["investing"]), -2000.0), "{flow}");
    assert!(close(dec(&q1["financing"]), 6500.0), "{flow}");
    assert!(close(dec(&q1["net_change"]), 5100.0), "{flow}");
    assert_eq!(q1["reconciles"], true, "{flow}");
    assert!(
        close(dec(&q1["cash_at_end"]) - dec(&q1["cash_at_start"]), 5100.0),
        "{flow}"
    );
    // A year earlier.
    let q1_2025 = &flow["columns"][1];
    assert_eq!(q1_2025["from"], "2025-01-01");
    assert!(close(dec(&q1_2025["net_income"]), 1100.0), "{flow}");
    assert!(close(dec(&q1_2025["net_change"]), -400.0), "{flow}");
    assert_eq!(q1_2025["reconciles"], true, "{flow}");
    let receivables = flow["operating"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == "1200")
        .unwrap();
    assert!(close(dec(&receivables["amounts"][0]), -2000.0), "{flow}");
    assert!(close(dec(&receivables["amounts"][1]), -1500.0), "{flow}");

    // Per month, each reconciles and they add up to the quarter.
    let monthly = run(
        &t,
        "gl.cash_flow",
        json!({ "company_id": b.company, "from": "2026-01-01", "to": "2026-03-31",
                "by_month": true }),
    )
    .await;
    let columns = monthly["columns"].as_array().unwrap();
    assert_eq!(columns.len(), 3);
    assert!(columns.iter().all(|c| c["reconciles"] == true), "{monthly}");
    let total: f64 = columns.iter().map(|c| dec(&c["net_change"])).sum();
    assert!(close(total, 5100.0), "{monthly}");
    t.stop().await;
}
