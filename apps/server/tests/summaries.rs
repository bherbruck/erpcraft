//! What a document's summary copies from the records it links to is shown
//! only to whoever may read those records.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

#[tokio::test]
async fn a_party_name_in_a_summary_follows_the_partys_read_rules() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Secret Supplies" }))
        .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({ "company_id": c.id, "doc_date": "2026-03-15", "body": {
                "vendor_id": vendor,
                "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "1", "unit_price": "2" }],
            } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    assert_eq!(draft["summary"]["vendor_name"], "Secret Supplies");

    // A clerk who may read orders, not vendors.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["purchase_order:read"]),
            Some(&c.id),
        )
        .await;
    let (status, doc) = t
        .get(
            &clerk,
            &format!("/api/documents/purchase_order/{}", id(&draft)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["summary"]["vendor_name"], Value::Null, "{doc}");
    assert_eq!(doc["summary"]["vendor_id"], vendor.as_str());
    let (status, list) = t
        .command(
            &clerk,
            "document.list",
            json!({ "doc_type": "purchase_order" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"][0]["summary"]["vendor_name"], Value::Null);
    assert!(!list.to_string().contains("Secret"));
    // Nor can the clerk find it out by filtering or sorting by it.
    for input in [
        json!({ "doc_type": "purchase_order", "summary": { "vendor_name": "Secret Supplies" } }),
        json!({ "doc_type": "purchase_order", "sort": ["summary.vendor_name"] }),
    ] {
        let (status, body) = t.command(&clerk, "document.list", input.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{input}: {body}");
    }
    // Other summary fields stay usable.
    let (status, body) = t
        .command(
            &clerk,
            "document.list",
            json!({ "doc_type": "purchase_order", "sort": ["-summary.total_net"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A buyer who may read the vendor, but not its name.
    let (buyer, _) = t
        .user_with_rules(
            "buyer@example.com",
            json!(["purchase_order:read", "vendor:read", { "deny": "vendor:read", "fields": ["name"] }]),
            Some(&c.id),
        )
        .await;
    let (_, doc) = t
        .get(
            &buyer,
            &format!("/api/documents/purchase_order/{}", id(&draft)),
        )
        .await;
    assert_eq!(doc["summary"]["vendor_name"], Value::Null, "{doc}");

    // Whoever reads vendors sees the name, and may filter by it.
    let (reader, _) = t
        .user_with_rules(
            "reader@example.com",
            json!(["purchase_order:read", "vendor:read"]),
            Some(&c.id),
        )
        .await;
    let (_, doc) = t
        .get(
            &reader,
            &format!("/api/documents/purchase_order/{}", id(&draft)),
        )
        .await;
    assert_eq!(doc["summary"]["vendor_name"], "Secret Supplies");
    let (status, list) = t
        .command(
            &reader,
            "document.list",
            json!({ "doc_type": "purchase_order", "summary": { "vendor_name": "Secret Supplies" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 1);
    t.stop().await;
}
