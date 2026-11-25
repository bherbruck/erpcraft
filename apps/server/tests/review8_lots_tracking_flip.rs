//! Review 8: an entry with lots reversed after its item stopped being
//! tracked brings lot levels back for an untracked item; once the item is
//! tracked again, lots on hand no longer agree with the item's stock.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};

use serde_json::{Value, json};

async fn adjust(
    t: &TestApp,
    company: &str,
    offset: &str,
    item: &str,
    warehouse: &str,
    qty: &str,
    lots: Value,
) -> (StatusCode, Value) {
    let cost = (!qty.starts_with('-')).then_some("2.00");
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/stock_adjustment",
            json!({ "company_id": company, "doc_date": "2026-03-01", "body": {
                "offset_account_id": offset, "lines": [{
                "item_id": item, "warehouse_id": warehouse, "qty": qty, "unit_cost": cost,
                "lots": lots }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    t.command(
        &t.admin,
        "stock_adjustment.post",
        json!({ "id": id(&draft) }),
    )
    .await
}

#[tokio::test]
async fn lot_levels_agree_with_stock_after_tracking_flips() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let account = |code: &'static str, kind: &'static str| {
        let company = c.id.clone();
        let t = &t;
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": code, "account_type": kind }),
            )
            .await
        }
    };
    let inventory = account("1300", "asset").await;
    let offset = account("6000", "expense").await;
    t.inventory_rule(&c.id, &inventory).await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let flour = t
        .create(
            "item",
            json!({ "code": "FLOUR", "name": "Flour", "uom": "kg", "tracking": "lot" }),
        )
        .await;

    // 5 of lot F-1 in, then out.
    let (s, b) = adjust(
        &t,
        &c.id,
        &offset,
        &flour,
        &main,
        "5",
        json!([{ "lot": "F-1", "qty": "5" }]),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, out) = adjust(
        &t,
        &c.id,
        &offset,
        &flour,
        &main,
        "-5",
        json!([{ "lot": "F-1", "qty": "5" }]),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");

    // No stock anywhere: tracking may change.
    let (s, b) = t
        .patch(
            &t.admin,
            &format!("/api/entities/item/{flour}"),
            json!({ "tracking": "none" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{b}");

    // Taking back the issue brings 5 back, with lot F-1, for an untracked item.
    let (s, b) = t
        .command(
            &t.admin,
            "stock_adjustment.reverse",
            json!({ "id": id(&out) }),
        )
        .await;
    // Either the ledger refuses this reversal, or it should not bring lot
    // levels back for an untracked item. Record what happened.
    eprintln!("reverse: {s} {b}");
    if s == StatusCode::OK {
        // 5 untracked out: stock 0.
        let (s, b) = adjust(&t, &c.id, &offset, &flour, &main, "-5", json!([])).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        // Tracked again: no stock, so allowed.
        let (s, b) = t
            .patch(
                &t.admin,
                &format!("/api/entities/item/{flour}"),
                json!({ "tracking": "lot" }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let item_qty: f64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(qty), 0)::float8 FROM stock_level WHERE item_id = $1::uuid",
        )
        .bind(&flour)
        .fetch_one(&t.pool)
        .await
        .unwrap();
        let (s, lots) = t
            .command(
                &t.admin,
                "stock.lots",
                json!({ "company_id": c.id, "item_id": flour }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{lots}");
        let lot_qty: f64 = lots["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| dec(&r["qty"]))
            .sum();
        eprintln!("item on hand {item_qty}, lots on hand {lot_qty}: {lots}");

        // With stock below zero allowed, the phantom lot can be issued and
        // the tracked item goes below zero.
        t.create(
            "policy_rule",
            json!({ "company_id": c.id, "point": "stock.negative", "effect": "allow", "condition": null }),
        )
        .await;
        let (s, b) = adjust(
            &t,
            &c.id,
            &offset,
            &flour,
            &main,
            "-5",
            json!([{ "lot": "F-1", "qty": "5" }]),
        )
        .await;
        let after: f64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(qty), 0)::float8 FROM stock_level WHERE item_id = $1::uuid",
        )
        .bind(&flour)
        .fetch_one(&t.pool)
        .await
        .unwrap();
        eprintln!("issue of phantom lot: {s} {b}; item on hand now {after}");
        assert_eq!(
            lot_qty, item_qty,
            "lots on hand disagree with the item's stock"
        );
        assert!(after >= 0.0, "a tracked item went below zero: {after}");
    }
    t.stop().await;
}
