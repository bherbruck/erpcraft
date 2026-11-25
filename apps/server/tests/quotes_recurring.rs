//! Sales quotes priced from the price lists, won into a sales order or
//! lost with a reason; and recurring invoices made on a schedule, drafted
//! or posted as the company's policy allows.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

/// A company with a customer; its id, the customer's and the setup's
/// answer.
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

#[tokio::test]
async fn a_quote_is_priced_issued_and_won_into_an_order_or_lost() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let warehouse = created["warehouse_id"].clone();
    let widget = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let list = t
        .create(
            "price_list",
            json!({ "company_id": c, "code": "BASE", "name": "Base", "side": "sales",
                    "currency": "USD" }),
        )
        .await;
    t.create(
        "price_list_line",
        json!({ "price_list_id": list, "item_id": widget, "unit_price": "10.00" }),
    )
    .await;
    let quote = |price: Option<&str>| {
        let mut line = json!({ "item_id": widget, "warehouse_id": warehouse, "qty": "5" });
        if let Some(p) = price {
            line["unit_price"] = json!(p);
        }
        json!({ "company_id": c, "doc_date": "2026-03-01", "body": {
            "customer_id": customer, "valid_until": "2026-03-31", "lines": [line] } })
    };
    let (status, draft) = t
        .post(&t.admin, "/api/documents/sales_quote", quote(None))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["unit_price"]), 10.0);
    assert_eq!(dec(&draft["summary"]["total_net"]), 50.0);
    let issued = run(&t, "sales_quote.issue", json!({ "id": id(&draft) })).await;
    assert_eq!(issued["status"], "issued");
    assert!(
        issued["number"].as_str().unwrap().starts_with("SQ-"),
        "{issued}"
    );

    // Won: a draft sales order at the quote's prices, linked to it.
    let order = run(
        &t,
        "sales_quote.accept",
        json!({ "id": id(&draft), "reason": "Best delivery time", "order_date": "2026-03-20" }),
    )
    .await;
    assert_eq!(order["doc_type"], "sales_order");
    assert_eq!(order["status"], "draft");
    assert_eq!(dec(&order["body"]["lines"][0]["unit_price"]), 10.0);
    assert_eq!(order["body"]["customer_id"], json!(customer));
    let (_, won) = t
        .get(
            &t.admin,
            &format!("/api/documents/sales_quote/{}", id(&draft)),
        )
        .await;
    assert_eq!(won["status"], "won");
    assert_eq!(won["body"]["outcome"], "won");
    assert_eq!(won["body"]["reason"], "Best delivery time");
    assert_eq!(won["body"]["order_id"], order["id"]);
    run(&t, "sales_order.approve", json!({ "id": id(&order) })).await;
    // A won quote is not accepted again.
    let (status, _) = t
        .command(&t.admin, "sales_quote.accept", json!({ "id": id(&draft) }))
        .await;
    assert_ne!(status, StatusCode::OK);

    // Lost, with a reason; it needs one.
    let (_, second) = t
        .post(&t.admin, "/api/documents/sales_quote", quote(Some("9.00")))
        .await;
    run(&t, "sales_quote.issue", json!({ "id": id(&second) })).await;
    let (status, _) = t
        .command(
            &t.admin,
            "sales_quote.lose",
            json!({ "id": id(&second), "reason": " " }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let lost = run(
        &t,
        "sales_quote.lose",
        json!({ "id": id(&second), "reason": "A competitor was cheaper" }),
    )
    .await;
    assert_eq!(lost["status"], "lost");
    assert_eq!(lost["body"]["reason"], "A competitor was cheaper");

    // Expired: refused by default, allowed for a week by a rule.
    let (_, third) = t
        .post(&t.admin, "/api/documents/sales_quote", quote(None))
        .await;
    run(&t, "sales_quote.issue", json!({ "id": id(&third) })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "sales_quote.accept",
            json!({ "id": id(&third), "order_date": "2026-04-05" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.to_string().contains("expired"), "{body}");
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "sales_quote.expired", "effect": "allow",
                "condition": "facts.days_expired <= 7" }),
    )
    .await;
    run(
        &t,
        "sales_quote.accept",
        json!({ "id": id(&third), "order_date": "2026-04-05" }),
    )
    .await;
    t.stop().await;
}

#[tokio::test]
async fn recurring_invoices_are_made_on_schedule_and_posted_as_the_policy_allows() {
    let t = TestApp::start().await;
    let (c, customer, created) = shop(&t).await;
    let revenue = created["roles"]["revenue"]["id"].clone();
    // Monthly from January 31, three in all, posted.
    let rent = t
        .create(
            "recurring_invoice",
            json!({ "company_id": c, "code": "RENT", "name": "Equipment rental",
                    "customer_id": customer, "frequency": "monthly", "starts_on": "2026-01-31",
                    "max_count": 3, "post": true }),
        )
        .await;
    t.create(
        "recurring_invoice_line",
        json!({ "recurring_invoice_id": rent, "account_id": revenue, "description": "Rental",
                "qty": "1", "unit_price": "100.00" }),
    )
    .await;
    // Quarterly from January 1 until June, left as drafts.
    let care = t
        .create(
            "recurring_invoice",
            json!({ "company_id": c, "code": "CARE", "name": "Maintenance",
                    "customer_id": customer, "frequency": "quarterly", "starts_on": "2026-01-01",
                    "end_date": "2026-06-30" }),
        )
        .await;
    t.create(
        "recurring_invoice_line",
        json!({ "recurring_invoice_id": care, "account_id": revenue, "description": "Care",
                "unit_price": "40.00" }),
    )
    .await;

    let made = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-03-15" }),
    )
    .await;
    let rows: Vec<(String, String, bool)> = made["invoices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["code"].as_str().unwrap().to_owned(),
                m["invoice_date"].as_str().unwrap().to_owned(),
                m["posted"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("CARE".into(), "2026-01-01".into(), false),
            ("RENT".into(), "2026-01-31".into(), true),
            ("RENT".into(), "2026-02-28".into(), true),
        ],
        "{made}"
    );
    let invoice = made["invoices"][1]["invoice_id"].as_str().unwrap();
    let (_, doc) = t
        .get(&t.admin, &format!("/api/documents/sales_invoice/{invoice}"))
        .await;
    assert_eq!(doc["status"], "posted", "{doc}");
    assert_eq!(dec(&doc["summary"]["total_net"]), 100.0);
    // Run again for the same day: nothing twice.
    let again = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-03-15" }),
    )
    .await;
    assert_eq!(again["invoices"], json!([]), "{again}");
    let (_, template) = t
        .get(&t.admin, &format!("/api/entities/recurring_invoice/{rent}"))
        .await;
    assert_eq!(template["next_date"], "2026-03-31", "{template}");
    assert_eq!(template["count"], 2);

    // The company leaves invoices over 50 for a person to post.
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "recurring_invoice.post", "effect": "refuse",
                "condition": "facts.total > 50" }),
    )
    .await;
    let made = run(
        &t,
        "recurring_invoice.run",
        json!({ "company_id": c, "as_of": "2026-12-31" }),
    )
    .await;
    let rows: Vec<(String, String, bool)> = made["invoices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["code"].as_str().unwrap().to_owned(),
                m["invoice_date"].as_str().unwrap().to_owned(),
                m["posted"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("CARE".into(), "2026-04-01".into(), false),
            ("RENT".into(), "2026-03-31".into(), false),
        ],
        "{made}"
    );
    // Both are done: three rentals, and the maintenance ended in June.
    for template in [&rent, &care] {
        let (_, record) = t
            .get(
                &t.admin,
                &format!("/api/entities/recurring_invoice/{template}"),
            )
            .await;
        assert_eq!(record["active"], false, "{record}");
    }
    t.stop().await;
}
