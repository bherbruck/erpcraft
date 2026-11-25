//! Review 8: fiscal_period.split_months, monthly setup and as-of open items.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn draft(t: &TestApp, c: &str, doc_type: &str, date: &str, body: Value) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": c, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    draft
}

fn balance(tb: &Value, code: &str) -> f64 {
    tb["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["code"] == code)
        .map(|r| dec(&r["balance"]))
        .sum()
}

/// Two adjacent open periods that share a calendar month (a 52/53-week or
/// mid-month fiscal year): dividing both gives two months named the same
/// YYYY-MM, and fiscal_period has UNIQUE (company_id, name).
#[tokio::test]
async fn review8_periods_split_adjacent_years_sharing_a_month() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "R8P", "name": "R8 Periods",
                "first_year": 2026, "years": 1, "periods": "yearly" }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let first = t
        .create(
            "fiscal_period",
            json!({ "company_id": c, "name": "FY2027", "start_date": "2027-01-01",
                    "end_date": "2027-06-15" }),
        )
        .await;
    let second = t
        .create(
            "fiscal_period",
            json!({ "company_id": c, "name": "FY2027b", "start_date": "2027-06-16",
                    "end_date": "2027-12-31" }),
        )
        .await;
    run(&t, "fiscal_period.split_months", json!({ "id": first })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "fiscal_period.split_months",
            json!({ "id": second }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the second half-year, June 16 to December 31, is not divided: {body}"
    );
    t.stop().await;
}

/// The default is now monthly periods. A year-end close that names only
/// its fiscal_period_id (as it did when a year was one period) closes only
/// December; the income of January to November stays in the income
/// accounts with no warning.
#[tokio::test]
async fn review8_periods_year_end_close_of_one_month_in_a_monthly_year() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "R8Y", "name": "R8 Year",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let bank = created["roles"]["bank"]["id"].as_str().unwrap().to_owned();
    let revenue = created["roles"]["revenue"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (date, amount) in [("2026-03-15", "100.00"), ("2026-12-15", "1.00")] {
        let d = draft(
            &t,
            &c,
            "journal_entry",
            date,
            json!({ "lines": [
                { "account_id": bank, "debit": amount },
                { "account_id": revenue, "credit": amount } ] }),
        )
        .await;
        run(&t, "journal_entry.post", json!({ "id": id(&d) })).await;
    }
    let year = &created["fiscal_years"][0];
    let d = draft(
        &t,
        &c,
        "year_end_close",
        "2026-12-31",
        json!({ "fiscal_period_id": year["last_period_id"],
                "retained_earnings_account_id": created["roles"]["retained_earnings"]["id"] }),
    )
    .await;
    let (status, posted) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&d) }))
        .await;
    if status == StatusCode::OK {
        assert!(
            (dec(&posted["summary"]["result"]) - 101.0).abs() < 0.005,
            "a year-end close of a monthly year naming only December closed {} of the \
             year's 101.00: {posted}",
            posted["summary"]["result"]
        );
    }
    t.stop().await;
}

/// A payment dated before the invoice it settles: as of a day between the
/// two, the ledger's receivable holds the payment's credit, but the as-of
/// open items list neither the invoice (posted later) nor any credit.
#[tokio::test]
async fn review8_periods_as_of_payment_dated_before_its_invoice() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "R8O", "name": "R8 Open",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let bank = created["roles"]["bank"]["id"].as_str().unwrap().to_owned();
    let revenue = created["roles"]["revenue"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let receivable = created["roles"]["receivable"]["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let customer = t
        .create("customer", json!({ "code": "ACME", "name": "Acme" }))
        .await;
    let inv = draft(
        &t,
        &c,
        "sales_invoice",
        "2026-03-20",
        json!({ "customer_id": customer, "lines": [
            { "account_id": revenue, "qty": "1", "unit_price": "100.00" } ] }),
    )
    .await;
    run(&t, "sales_invoice.post", json!({ "id": id(&inv) })).await;
    let open = run(
        &t,
        "open_item.list",
        json!({ "company_id": c, "kind": "receivable" }),
    )
    .await;
    let item = open["items"][0]["id"].clone();
    let pay = draft(
        &t,
        &c,
        "payment",
        "2026-03-10",
        json!({ "direction": "incoming", "bank_account_id": bank, "allocations": [
            { "open_item_id": item, "amount": "100.00" } ] }),
    )
    .await;
    let (status, body) = t
        .command(&t.admin, "payment.post", json!({ "id": id(&pay) }))
        .await;
    if status != StatusCode::OK {
        // Refused: a payment cannot predate its item. Nothing to compare.
        eprintln!("payment before the invoice refused: {body}");
        t.stop().await;
        return;
    }
    let listed = run(
        &t,
        "open_item.list",
        json!({ "company_id": c, "kind": "receivable", "as_of": "2026-03-15" }),
    )
    .await;
    let ledger = run(
        &t,
        "gl.trial_balance",
        json!({ "company_id": c, "as_of": "2026-03-15" }),
    )
    .await;
    assert!(
        (dec(&listed["total_carried"]) - balance(&ledger, &receivable)).abs() < 0.005,
        "on March 15 the ledger's receivable is {}, the open items as of then say {}: {listed}",
        balance(&ledger, &receivable),
        listed["total_carried"]
    );
    t.stop().await;
}
