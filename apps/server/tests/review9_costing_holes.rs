//! Review 9, standard costing: ways an item kept at standard ends up with
//! stock that is not worth its standard, or goes to standard with stock and
//! no revaluation.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct Works {
    c: String,
    plant: String,
    vendor: String,
    steel: String,
}

async fn works(t: &TestApp) -> Works {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_manufacturer", "code": "WORKS", "name": "Works Inc",
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let c = body["company"]["id"].as_str().unwrap().to_owned();
    let account = t
        .create(
            "account",
            json!({ "company_id": c, "code": "5071", "name": "Revaluation",
                    "account_type": "expense" }),
        )
        .await;
    t.create(
        "account_rule",
        json!({ "company_id": c, "purpose": "standard_cost_revaluation", "account_id": account }),
    )
    .await;
    let plant = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "PLANT", "name": "Plant" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "MILL", "name": "Steel Mill" }))
        .await;
    let steel = t
        .create("item", json!({ "code": "STEEL", "name": "Steel" }))
        .await;
    Works {
        c,
        plant,
        vendor,
        steel,
    }
}

impl Works {
    async fn draft(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    async fn ok(&self, t: &TestApp, command: &str, input: Value) -> Value {
        let (status, body) = t.command(&t.admin, command, input).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    async fn buy(&self, t: &TestApp, qty: &str, price: &str, date: &str) -> Value {
        let order = self
            .draft(
                t,
                "purchase_order",
                date,
                json!({ "vendor_id": self.vendor, "lines": [
                    { "item_id": self.steel, "warehouse_id": self.plant, "qty": qty,
                      "unit_price": price }] }),
            )
            .await;
        let order = self
            .ok(t, "purchase_order.approve", json!({ "id": id(&order) }))
            .await;
        let receipt = self
            .draft(
                t,
                "purchase_receipt",
                date,
                json!({ "purchase_order_id": order["id"], "lines": [
                    { "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }] }),
            )
            .await;
        self.ok(t, "purchase_receipt.post", json!({ "id": id(&receipt) }))
            .await
    }

    async fn level(&self, t: &TestApp) -> (f64, f64) {
        let body = self
            .ok(
                t,
                "stock.on_hand",
                json!({ "company_id": self.c, "item_id": self.steel }),
            )
            .await;
        body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .fold((0.0, 0.0), |(q, v), r| {
                (q + dec(&r["qty"]), v + dec(&r["value"]))
            })
    }

    async fn activate(&self, t: &TestApp, date: &str, cost: &str, switch: bool) {
        let v = self
            .draft(
                t,
                "cost_version",
                date,
                json!({ "lines": [{ "item_id": self.steel, "unit_cost": cost,
                                    "material_cost": cost, "switch_to_standard": switch }] }),
            )
            .await;
        self.ok(t, "cost_version.activate", json!({ "id": id(&v) }))
            .await;
    }

    async fn method(&self, t: &TestApp) -> String {
        sqlx::query_scalar("SELECT stock_costing_method($1::uuid, $2::uuid)")
            .bind(&self.c)
            .bind(&self.steel)
            .fetch_one(&t.pool)
            .await
            .unwrap()
    }

    async fn standard(&self, t: &TestApp) -> Option<f64> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT unit_cost::text FROM item_standard WHERE company_id = $1::uuid AND item_id = $2::uuid",
        )
        .bind(&self.c)
        .bind(&self.steel)
        .fetch_optional(&t.pool)
        .await
        .unwrap();
        s.map(|s| s.parse().unwrap())
    }

    async fn verify(&self, t: &TestApp) {
        let report = erpcraft_server::verify::verify(&t.pool, Some("WORKS"))
            .await
            .unwrap();
        assert!(report.ok, "the books do not agree:\n{}", report.text());
    }
}

/// Deleting an item's own costing record moves it to the company's default
/// without the guard that an update or insert goes through: an item with
/// stock at average becomes a standard item, its stock never revalued.
#[tokio::test]
async fn deleting_an_items_costing_moves_it_to_standard_with_stock_unrevalued() {
    let t = TestApp::start().await;
    let w = works(&t).await;
    // Steel kept at average by its own record; the company's default is
    // standard (allowed: nothing following the default has stock).
    let costing = t
        .create(
            "item_costing",
            json!({ "company_id": w.c, "item_id": w.steel, "method": "average" }),
        )
        .await;
    t.create(
        "stock_costing",
        json!({ "company_id": w.c, "default_method": "standard" }),
    )
    .await;
    // A version lists steel (still at average, so nothing is revalued):
    // it now has a standard of 2.50 on record.
    w.activate(&t, "2026-03-01", "2.50", false).await;
    w.buy(&t, "100", "2.00", "2026-03-02").await;
    assert_eq!(w.method(&t).await, "average");
    assert_eq!(w.level(&t).await, (100.0, 200.0));

    let (status, body) = t
        .send(
            "DELETE",
            &format!("/api/entities/item_costing/{costing}"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    println!("delete: {status} {body}");
    let method = w.method(&t).await;
    let level = w.level(&t).await;
    w.verify(&t).await;
    // Either the delete is refused, or the item stayed at average, or its
    // stock was revalued to its standard.
    assert!(
        method == "average" || level.1 == level.0 * w.standard(&t).await.unwrap_or(-1.0),
        "steel is now {method} with stock {level:?}, standard {:?}: stock at standard \
         that was never revalued",
        w.standard(&t).await
    );
    t.stop().await;
}

/// A receipt taken back after a new standard takes back the old standard's
/// value, so the stock left is not worth its standard.
#[tokio::test]
async fn reversing_a_receipt_after_a_new_standard_leaves_stock_off_standard() {
    let t = TestApp::start().await;
    let w = works(&t).await;
    t.create(
        "item_costing",
        json!({ "company_id": w.c, "item_id": w.steel, "method": "standard" }),
    )
    .await;
    w.activate(&t, "2026-03-01", "5.00", false).await;
    let first = w.buy(&t, "10", "5.00", "2026-03-02").await;
    w.buy(&t, "10", "5.00", "2026-03-03").await;
    assert_eq!(w.level(&t).await, (20.0, 100.0));
    w.activate(&t, "2026-03-04", "6.00", false).await;
    assert_eq!(w.level(&t).await, (20.0, 120.0));

    let (status, body) = t
        .command(
            &t.admin,
            "purchase_receipt.reverse",
            json!({ "id": id(&first) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "reverse: {body}");
    w.verify(&t).await;
    if status == StatusCode::OK {
        // 10 left at a standard of 6.00.
        assert_eq!(
            w.level(&t).await,
            (10.0, 60.0),
            "the stock left after the reversal is not worth its standard"
        );
    }
    t.stop().await;
}

/// A landed cost raised the value of goods while the item was at average;
/// the item then went to standard (revalued). Reversing the invoice takes
/// the landed cost back off a standard item's value: a cost adjustment the
/// ledger otherwise refuses for a standard item.
#[tokio::test]
async fn reversing_a_landed_cost_after_switching_to_standard_takes_value_off_a_standard_item() {
    let t = TestApp::start().await;
    let w = works(&t).await;
    let receipt = w.buy(&t, "10", "2.00", "2026-03-02").await;
    let freight = t
        .create(
            "charge",
            json!({ "code": "FREIGHT", "name": "Freight", "kind": "freight", "landed_cost": true }),
        )
        .await;
    let bill = w
        .draft(
            &t,
            "vendor_invoice",
            "2026-03-03",
            json!({ "vendor_id": w.vendor, "lines": [
                { "charge_id": freight, "qty": "1", "unit_price": "12.00",
                  "receipt_ids": [receipt["id"]] }] }),
        )
        .await;
    w.ok(&t, "vendor_invoice.post", json!({ "id": id(&bill) }))
        .await;
    assert_eq!(w.level(&t).await, (10.0, 32.0));
    // To standard at 3.00, revalued from 32.00 to 30.00.
    w.activate(&t, "2026-03-04", "3.00", true).await;
    assert_eq!(w.method(&t).await, "standard");
    assert_eq!(w.level(&t).await, (10.0, 30.0));

    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.reverse",
            json!({ "id": id(&bill) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "reverse invoice: {body}");
    w.verify(&t).await;
    if status == StatusCode::OK {
        assert_eq!(
            w.level(&t).await,
            (10.0, 30.0),
            "a standard item's stock lost value to a reversed landed cost"
        );
    }
    t.stop().await;
}

/// The guard that keeps an item with stock from going to standard reads
/// the levels without locking them: a receipt committing while the switch
/// is in flight leaves a standard item with stock never revalued (here, and
/// with no standard at all, so it cannot move).
#[tokio::test]
async fn switching_to_standard_races_a_receipt() {
    let t = TestApp::start().await;
    let w = works(&t).await;
    let mut tx = t.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO item_costing (id, company_id, item_id, method)
         VALUES (gen_random_uuid(), $1::uuid, $2::uuid, 'standard')",
    )
    .bind(&w.c)
    .bind(&w.steel)
    .execute(&mut *tx)
    .await
    .unwrap();
    let switch = async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        tx.commit().await
    };
    let receive = async {
        let order = w
            .draft(
                &t,
                "purchase_order",
                "2026-03-02",
                json!({ "vendor_id": w.vendor, "lines": [
                    { "item_id": w.steel, "warehouse_id": w.plant, "qty": "10",
                      "unit_price": "7.00" }] }),
            )
            .await;
        let order = w
            .ok(&t, "purchase_order.approve", json!({ "id": id(&order) }))
            .await;
        let receipt = w
            .draft(
                &t,
                "purchase_receipt",
                "2026-03-02",
                json!({ "purchase_order_id": order["id"], "lines": [
                    { "order_line_id": order["body"]["lines"][0]["id"], "qty": "10" }] }),
            )
            .await;
        t.command(
            &t.admin,
            "purchase_receipt.post",
            json!({ "id": id(&receipt) }),
        )
        .await
    };
    let (committed, (status, body)) = tokio::join!(switch, receive);
    println!("switch committed: {committed:?}; receipt: {status} {body}");
    let method = w.method(&t).await;
    let level = w.level(&t).await;
    let standard = w.standard(&t).await;
    w.verify(&t).await;
    assert!(
        !(method == "standard" && level.0 != 0.0 && standard.map(|s| s * level.0) != Some(level.1)),
        "steel is {method} with stock {level:?} and standard {standard:?}"
    );
    t.stop().await;
}
