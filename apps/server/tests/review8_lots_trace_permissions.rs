//! Review 8: stock.trace takes only stock_entry:read, yet returns the
//! vendors and customers of the documents that moved a lot, with their
//! names and the documents' numbers, to someone who may read none of
//! them. stock.reservations hides the order from such a caller.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::json;

#[tokio::test]
async fn a_trace_shows_parties_only_to_who_may_read_them() {
    let t = TestApp::start().await;
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_manufacturer", "code": "BAKE", "name": "Bakery Inc",
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let c = body["company"]["id"].as_str().unwrap().to_owned();
    let plant = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "PLANT", "name": "Plant" }),
        )
        .await;
    let vendor = t
        .create(
            "vendor",
            json!({ "code": "MILL", "name": "Secret Mill Ltd" }),
        )
        .await;
    let flour = t
        .create(
            "item",
            json!({ "code": "FLOUR", "name": "Flour", "uom": "kg", "tracking": "lot" }),
        )
        .await;
    let (s, order) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({ "company_id": c, "doc_date": "2026-03-01", "body": {
                "vendor_id": vendor, "lines": [
                { "item_id": flour, "warehouse_id": plant, "qty": "10", "unit_price": "1.00" }] } }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{order}");
    let (s, order) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": id(&order) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{order}");
    let (s, receipt) = t
        .post(
            &t.admin,
            "/api/documents/purchase_receipt",
            json!({ "company_id": c, "doc_date": "2026-03-02", "body": {
                "purchase_order_id": order["id"], "lines": [
                { "order_line_id": order["body"]["lines"][0]["id"], "qty": "10",
                  "lots": [{ "lot": "FL-1", "qty": "10" }] }] } }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{receipt}");
    let (s, b) = t
        .command(
            &t.admin,
            "purchase_receipt.post",
            json!({ "id": id(&receipt) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");

    // A stock clerk: may read the stock ledger, nothing about vendors or
    // purchasing.
    let clerk = t
        .user_with_role("clerk@example.com", &["stock_entry:read"], &c)
        .await;
    let (s, vendors) = t.get(&clerk, "/api/entities/vendor").await;
    eprintln!("clerk reads vendors: {s} {vendors}");
    let (s, receipt_view) = t
        .get(
            &clerk,
            &format!("/api/documents/purchase_receipt/{}", id(&receipt)),
        )
        .await;
    eprintln!("clerk reads the receipt: {s}");
    assert_ne!(
        s,
        StatusCode::OK,
        "the clerk may not read purchase receipts: {receipt_view}"
    );

    let (s, trace) = t
        .command(
            &clerk,
            "stock.trace",
            json!({ "company_id": c, "item_id": flour, "lot": "FL-1", "direction": "backward" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{trace}");
    let text = trace.to_string();
    eprintln!("{trace}");
    assert!(
        !text.contains("Secret Mill Ltd"),
        "the trace names a vendor to someone who may not read vendors or receipts: {trace}"
    );
    t.stop().await;
}
