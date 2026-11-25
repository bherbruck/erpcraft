//! Review 8: price lists, the override policy and who may read and write
//! them.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn shop(t: &TestApp, code: &str) -> (String, Value) {
    let created = run(
        t,
        "company.setup",
        json!({ "template": "us_small_business", "code": code, "name": code,
                "first_year": 2026 }),
    )
    .await;
    (
        created["company"]["id"].as_str().unwrap().to_owned(),
        created["warehouse_id"].clone(),
    )
}

#[tokio::test]
async fn a_discount_amount_is_not_put_to_the_override_policy() {
    let t = TestApp::start().await;
    let (company, warehouse) = shop(&t, "OHIO").await;
    let widget = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let cash = t
        .create("customer", json!({ "code": "CASH", "name": "Walk-in" }))
        .await;
    let list = t
        .create(
            "price_list",
            json!({ "company_id": company, "code": "BASE", "name": "Base", "side": "sales",
                    "currency": "USD" }),
        )
        .await;
    t.create(
        "price_list_line",
        json!({ "price_list_id": list, "item_id": widget, "unit_price": "10.00" }),
    )
    .await;
    t.create(
        "policy_rule",
        json!({ "company_id": company, "point": "price.override", "effect": "refuse",
                "condition": "facts.change_percent < -2", "message": "below the price list" }),
    )
    .await;
    // The list price, with 45 of the 50 taken off as an amount: 90 % off.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": company, "doc_date": "2026-02-10", "body": {
                "customer_id": cash, "lines": [{ "item_id": widget, "warehouse_id": warehouse,
                    "qty": "5", "discount_amount": "45.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = t
        .command(&t.admin, "sales_order.approve", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "90 % off the list through discount_amount was approved: {body}"
    );
    t.stop().await;
}

#[tokio::test]
async fn price_list_lines_of_one_company_are_read_by_another() {
    let t = TestApp::start().await;
    let (ohio, _) = shop(&t, "OHIO").await;
    let (other, _) = shop(&t, "OTHER").await;
    let widget = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let list = t
        .create(
            "price_list",
            json!({ "company_id": ohio, "code": "SECRET", "name": "Secret", "side": "purchase",
                    "currency": "USD" }),
        )
        .await;
    t.create(
        "price_list_line",
        json!({ "price_list_id": list, "item_id": widget, "unit_price": "3.21" }),
    )
    .await;
    // Someone who may read OTHER's price lists only.
    let token = t
        .user_with_role(
            "buyer@other.test",
            &["price_list:read", "price_list_line:read"],
            &other,
        )
        .await;
    let (status, body) = t.get(&token, "/api/entities/price_list_line").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.to_string().contains("3.21"),
        "OHIO's purchase price is read by OTHER's buyer: {body}"
    );
    t.stop().await;
}

#[tokio::test]
async fn a_company_pricing_manager_cannot_add_a_line_to_its_own_list() {
    let t = TestApp::start().await;
    let (ohio, _) = shop(&t, "OHIO").await;
    let widget = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let list = t
        .create(
            "price_list",
            json!({ "company_id": ohio, "code": "BASE", "name": "Base", "side": "sales",
                    "currency": "USD" }),
        )
        .await;
    let token = t
        .user_with_role(
            "pricing@ohio.test",
            &[
                "price_list:read",
                "price_list:create",
                "price_list_line:read",
                "price_list_line:create",
            ],
            &ohio,
        )
        .await;
    let (status, body) = t
        .post(
            &token,
            "/api/entities/price_list_line",
            json!({ "price_list_id": list, "item_id": widget, "unit_price": "10.00" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    t.stop().await;
}
