//! Review 8: the last price paid, per stock unit and back to a purchase
//! unit.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

#[tokio::test]
async fn the_last_price_paid_per_box_comes_back_as_it_was_paid() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio",
                "first_year": 2026 }),
    )
    .await;
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let warehouse = created["warehouse_id"].clone();
    let widget = t
        .create(
            "item",
            json!({ "code": "W", "name": "Widget", "uom": "each" }),
        )
        .await;
    // Bought by the box of 3.
    t.create(
        "item_uom",
        json!({ "item_id": widget, "uom": "box", "factor": "3" }),
    )
    .await;
    let vendor = t
        .create("vendor", json!({ "code": "V", "name": "Vendor" }))
        .await;
    let order = |price: Option<&str>| {
        let mut line = json!({ "item_id": widget, "warehouse_id": warehouse, "qty": "2",
                               "uom": "box" });
        if let Some(price) = price {
            line["unit_price"] = json!(price);
        }
        json!({ "company_id": company, "doc_date": "2026-03-01",
                "body": { "vendor_id": vendor, "lines": [line] } })
    };
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            order(Some("10.00")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let po = run(&t, "purchase_order.approve", json!({ "id": id(&draft) })).await;
    let line = &po["body"]["lines"][0]["id"];
    for (doc_type, body) in [
        (
            "purchase_receipt",
            json!({ "purchase_order_id": po["id"], "lines": [{ "order_line_id": line, "qty": "2" }] }),
        ),
        (
            "vendor_invoice",
            json!({ "purchase_order_id": po["id"],
                    "lines": [{ "order_line_id": line, "qty": "2", "unit_price": "10.00" }] }),
        ),
    ] {
        let (status, d) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": company, "doc_date": "2026-03-05", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{d}");
        run(&t, &format!("{doc_type}.post"), json!({ "id": id(&d) })).await;
    }
    // Ordered again by the box: proposed what a box was paid.
    let (status, next) = t
        .post(&t.admin, "/api/documents/purchase_order", order(None))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{next}");
    assert_eq!(
        dec(&next["body"]["lines"][0]["unit_price"]),
        10.0,
        "{}",
        next["body"]["lines"][0]
    );
    t.stop().await;
}
