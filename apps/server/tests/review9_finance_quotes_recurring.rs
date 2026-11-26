//! Review 9: sales quotes and recurring invoices.
#![allow(dead_code)]

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn shop(t: &TestApp) -> (String, String, Value) {
    let created = run(
        t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026 }),
    )
    .await;
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let customer = t
        .create("customer", json!({ "code": "ACME", "name": "Acme" }))
        .await;
    (company, customer, created)
}

async fn issued_quote(t: &TestApp, c: &str, customer: &str, revenue: &Value) -> String {
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_quote",
            json!({ "company_id": c, "doc_date": "2026-03-01", "body": {
                "customer_id": customer, "valid_until": "2026-03-31",
                "lines": [{ "account_id": revenue, "description": "Service", "qty": "1",
                            "unit_price": "100.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(t, "sales_quote.issue", json!({ "id": id(&draft) })).await;
    id(&draft)
}

/// The quote expired on 2026-03-31 and today is long after it; the policy
/// sales_quote.expired refuses by default. Naming an order date inside the
/// validity must not let the expired prices through.
#[tokio::test]
async fn an_expired_quote_is_not_won_by_backdating_the_order() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let quote = issued_quote(&t, &c, &customer, &revenue).await;
    let today = erpcraft_kernel::Utc::now().date_naive();
    assert!(today.to_string().as_str() > "2026-03-31");
    let (status, body) = t
        .command(
            &t.admin,
            "sales_quote.accept",
            json!({ "id": quote, "order_date": "2026-03-15" }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an expired quote was won by backdating its order: {body}"
    );
    t.stop().await;
}

/// The summary's `expired` is worked out when the summary is saved, and
/// is never refreshed when the date passes. Simulated by moving the
/// quote's date into the past behind the guard, as the passing of time
/// would.
#[tokio::test]
async fn a_quote_reads_expired_once_its_date_passes() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let today = erpcraft_kernel::Utc::now().date_naive();
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_quote",
            json!({ "company_id": c, "doc_date": today.to_string(), "body": {
                "customer_id": customer, "valid_until": today.to_string(),
                "lines": [{ "account_id": revenue, "description": "x", "qty": "1",
                            "unit_price": "1" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let issued = run(&t, "sales_quote.issue", json!({ "id": id(&draft) })).await;
    assert_eq!(issued["summary"]["expired"], false, "{issued}");
    // Time passes: the quote's date is now yesterday.
    let mut tx = t.pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE sales_quote DISABLE TRIGGER sales_quote_guard")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE sales_quote SET valid_until = valid_until - 1 WHERE document_id = $1::uuid",
    )
    .bind(id(&draft))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE sales_quote ENABLE TRIGGER sales_quote_guard")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (_, read) = t
        .get(
            &t.admin,
            &format!("/api/documents/sales_quote/{}", id(&draft)),
        )
        .await;
    assert_eq!(
        read["summary"]["expired"], true,
        "an issued quote past its date still reads not expired: {}",
        read["summary"]
    );
    t.stop().await;
}

/// The draft order a won quote made can be discarded like any draft (or
/// is refused with a clear state error), not a database error.
#[tokio::test]
async fn a_won_quotes_draft_order_can_be_discarded() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let quote = issued_quote(&t, &c, &customer, &revenue).await;
    let order = run(
        &t,
        "sales_quote.accept",
        json!({ "id": quote, "order_date": "2026-03-20" }),
    )
    .await;
    let (status, body) = t
        .send(
            "DELETE",
            &format!("/api/documents/sales_order/{}", id(&order)),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert!(
        status.is_success() || status == StatusCode::CONFLICT,
        "deleting the draft order of a won quote: {status} {body}"
    );
    t.stop().await;
}

/// A monthly template made January to March; it is then switched to
/// weekly. The next run must not go back and invoice weeks in January to
/// March again.
#[tokio::test]
async fn changing_a_templates_frequency_does_not_invoice_the_past_again() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let rent = t
        .create(
            "recurring_invoice",
            json!({ "company_id": c, "code": "RENT", "name": "Rent",
                    "customer_id": customer, "frequency": "monthly", "starts_on": "2026-01-01" }),
        )
        .await;
    t.create(
        "recurring_invoice_line",
        json!({ "recurring_invoice_id": rent, "account_id": revenue, "description": "Rent",
                "qty": "1", "unit_price": "100.00" }),
    )
    .await;
    let made = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-03-15" }),
    )
    .await;
    assert_eq!(made["invoices"].as_array().unwrap().len(), 3, "{made}");
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/recurring_invoice/{rent}"),
            json!({ "frequency": "weekly" }),
        )
        .await;
    if status != StatusCode::OK {
        // Refusing the change is a fine answer too.
        t.stop().await;
        return;
    }
    let _ = body;
    let made = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-04-01" }),
    )
    .await;
    let dates: Vec<&str> = made["invoices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["invoice_date"].as_str().unwrap())
        .collect();
    assert!(
        dates.iter().all(|d| *d >= "2026-04-01"),
        "after switching to weekly, the run invoiced past weeks again: {dates:?}"
    );
    t.stop().await;
}

/// Same with `every`: monthly every month, two made, then every 1 -> 3.
/// The next date must not skip what is due, nor jump; more importantly a
/// change of `every` down must not reinvoice a date in the past.
#[tokio::test]
async fn lowering_every_does_not_invoice_the_past_again() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let care = t
        .create(
            "recurring_invoice",
            json!({ "company_id": c, "code": "CARE", "name": "Care", "customer_id": customer,
                    "frequency": "monthly", "every": 3, "starts_on": "2026-01-01" }),
        )
        .await;
    t.create(
        "recurring_invoice_line",
        json!({ "recurring_invoice_id": care, "account_id": revenue, "description": "Care",
                "qty": "1", "unit_price": "40.00" }),
    )
    .await;
    // January, April, July.
    let made = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-07-15" }),
    )
    .await;
    assert_eq!(made["invoices"].as_array().unwrap().len(), 3, "{made}");
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/recurring_invoice/{care}"),
            json!({ "every": 1 }),
        )
        .await;
    if status != StatusCode::OK {
        t.stop().await;
        return;
    }
    let (status, made) = t
        .command(
            &t.admin,
            "recurring_invoice.run",
            json!({ "company_id": c, "as_of": "2026-10-15" }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the run fails for the whole company after a template's every changed: {made}"
    );
    let dates: Vec<&str> = made["invoices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["invoice_date"].as_str().unwrap())
        .collect();
    assert!(
        dates.iter().all(|d| *d > "2026-07-01"),
        "after every 3 -> 1, the run invoiced past months again: {dates:?}"
    );
    t.stop().await;
}

/// A template in the accounting currency tells the policy its currency,
/// as every other policy point does: a rule on USD invoices over 50 must
/// hold for it.
#[tokio::test]
async fn the_post_policy_sees_the_accounting_currency() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let rent = t
        .create(
            "recurring_invoice",
            json!({ "company_id": c, "code": "RENT", "name": "Rent", "post": true,
                    "customer_id": customer, "frequency": "monthly", "starts_on": "2026-01-01" }),
        )
        .await;
    t.create(
        "recurring_invoice_line",
        json!({ "recurring_invoice_id": rent, "account_id": revenue, "description": "Rent",
                "qty": "1", "unit_price": "100.00" }),
    )
    .await;
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "recurring_invoice.post", "effect": "refuse",
                "condition": "facts.currency == 'USD' && facts.total > 50" }),
    )
    .await;
    let made = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-01-15" }),
    )
    .await;
    assert_eq!(
        made["invoices"][0]["posted"], false,
        "a 100 USD invoice was posted past a rule on USD invoices over 50: {made}"
    );
    t.stop().await;
}

/// The customer is switched off and the company refuses invoices for
/// inactive customers (`customer.inactive`). The schedule still makes and
/// posts its invoice: a direct invoice never asks the policy.
#[tokio::test]
async fn a_schedule_does_not_post_to_a_customer_switched_off() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    let rent = t
        .create(
            "recurring_invoice",
            json!({ "company_id": c, "code": "RENT", "name": "Rent", "post": true,
                    "customer_id": customer, "frequency": "monthly", "starts_on": "2026-01-01" }),
        )
        .await;
    t.create(
        "recurring_invoice_line",
        json!({ "recurring_invoice_id": rent, "account_id": revenue, "description": "Rent",
                "qty": "1", "unit_price": "100.00" }),
    )
    .await;
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "customer.inactive", "effect": "refuse",
                "condition": "!facts.active" }),
    )
    .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/customer/{customer}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, made) = t
        .command(
            &t.admin,
            "recurring_invoice.run",
            json!({ "company_id": c, "as_of": "2026-01-15" }),
        )
        .await;
    assert!(
        status != StatusCode::OK || made["invoices"][0]["posted"] == false,
        "an invoice was posted to a customer the company switched off: {made}"
    );
    t.stop().await;
}
