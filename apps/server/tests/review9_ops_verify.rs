//! Review 9, operations: `erpcraft verify` giving a false green.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

/// `erpcraft verify --company OHIO` with a typo (or run against the wrong
/// database) checks nothing and says "the books agree", exit 0.
#[tokio::test]
async fn verify_of_a_company_that_does_not_exist_is_not_green() {
    let t = TestApp::start().await;
    let _c = t.company("REAL", "USD").await;
    let report = erpcraft_server::verify::verify(&t.pool, Some("NOPE"))
        .await
        .unwrap();
    let ok = report.ok;
    let text = report.text();
    t.stop().await;
    assert!(
        !ok,
        "verify of a company code that matches nothing reported success:\n{text}"
    );
}

/// The receivables account of the sales settings carries a balance that no
/// open item stands for (a manual journal entry onto it, as the docs name),
/// but it has no open items at all, so the open-items check never looks at
/// it and verify is green.
#[tokio::test]
async fn verify_checks_a_receivable_account_that_has_no_open_items() {
    let t = TestApp::start().await;
    let c = t.company("ARCO", "USD").await;
    let account = |code: &'static str, name: &'static str, kind: &'static str| {
        let company = c.id.clone();
        let t = &t;
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
            )
            .await
        }
    };
    let receivable = account("1200", "Accounts receivable", "asset").await;
    let cogs = account("5000", "Cost of goods sold", "expense").await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": receivable,
            "revenue_account_id": c.revenue,
            "cogs_account_id": cogs,
        }),
    )
    .await;
    let (status, doc) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({
                "company_id": c.id,
                "doc_date": "2026-03-15",
                "memo": "straight onto receivables",
                "body": { "lines": [
                    { "account_id": receivable, "debit": "100.00" },
                    { "account_id": c.revenue, "credit": "100.00" },
                ]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");

    let report = erpcraft_server::verify::verify(&t.pool, None)
        .await
        .unwrap();
    let ok = report.ok;
    let text = report.text();
    t.stop().await;
    assert!(
        !ok,
        "the receivables account holds 100.00 that no open item stands for, yet:\n{text}"
    );
}
