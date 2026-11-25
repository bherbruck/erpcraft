//! Units of measure: an item kept in one unit, bought and sold in others,
//! converted exactly into the stock unit for the stock ledger.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct Shop {
    c: String,
    plant: String,
    vendor: String,
    customer: String,
    steel: String,
    bolt: String,
}

async fn shop(t: &TestApp) -> Shop {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_manufacturer", "code": "UNIT", "name": "Units Inc",
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
        .create("vendor", json!({ "code": "MILL", "name": "Steel Mill" }))
        .await;
    let customer = t
        .create(
            "customer",
            json!({ "code": "SHOP", "name": "Hardware Shop" }),
        )
        .await;
    let steel = t
        .create(
            "item",
            json!({ "code": "STEEL", "name": "Steel", "uom": "kg" }),
        )
        .await;
    let bolt = t
        .create(
            "item",
            json!({ "code": "BOLT", "name": "Bolt", "uom": "each" }),
        )
        .await;
    // Steel is bought by the tonne; bolts sold by the box of 100.
    t.create(
        "item_uom",
        json!({ "item_id": steel, "uom": "t", "factor": "1000", "purchase": true,
                "sales": false }),
    )
    .await;
    t.create(
        "item_uom",
        json!({ "item_id": bolt, "uom": "box", "factor": "100", "purchase": false,
                "sales": true }),
    )
    .await;
    Shop {
        c,
        plant,
        vendor,
        customer,
        steel,
        bolt,
    }
}

impl Shop {
    async fn draft(&self, t: &TestApp, doc_type: &str, body: Value) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": self.c, "doc_date": "2026-03-02", "body": body }),
        )
        .await
    }

    async fn ok(&self, t: &TestApp, command: &str, input: Value) -> Value {
        let (status, body) = t.command(&t.admin, command, input).await;
        assert_eq!(status, StatusCode::OK, "{command}: {body}");
        body
    }

    async fn availability(&self, t: &TestApp, item: &str) -> Value {
        let body = self
            .ok(
                t,
                "stock.availability",
                json!({ "company_id": self.c, "item_id": item }),
            )
            .await;
        body["rows"][0].clone()
    }

    async fn on_hand(&self, t: &TestApp, item: &str) -> (f64, f64) {
        let body = self
            .ok(
                t,
                "stock.on_hand",
                json!({ "company_id": self.c, "item_id": item }),
            )
            .await;
        let row = &body["rows"][0];
        (dec(&row["qty"]), dec(&row["value"]))
    }
}

fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn steel_is_bought_by_the_tonne_and_kept_in_kilograms() {
    let t = TestApp::start().await;
    let s = shop(&t).await;
    // A unit the item does not have, and one it is not bought in: refused
    // on the line.
    for (uom, says) in [("lb", "has no unit lb"), ("box", "")] {
        let item = if uom == "box" { &s.bolt } else { &s.steel };
        let (status, body) = s
            .draft(
                &t,
                "purchase_order",
                json!({ "vendor_id": s.vendor, "lines": [
                    { "item_id": item, "warehouse_id": s.plant, "qty": "1", "uom": uom,
                      "unit_price": "1" }] }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["field"], "uom", "{body}");
        assert!(message(&body).contains(says), "{body}");
    }

    // 2.5 t at 1,200.00 a tonne.
    let (status, order) = s
        .draft(
            &t,
            "purchase_order",
            json!({ "vendor_id": s.vendor, "lines": [
                { "item_id": s.steel, "warehouse_id": s.plant, "qty": "2.5", "uom": "t",
                  "unit_price": "1200" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_eq!(dec(&order["body"]["lines"][0]["factor"]), 1000.0);
    assert_eq!(dec(&order["summary"]["total_net"]), 3000.0, "{order}");
    let order = s
        .ok(&t, "purchase_order.approve", json!({ "id": id(&order) }))
        .await;
    // Planning sees 2,500 kg coming.
    assert_eq!(dec(&s.availability(&t, &s.steel).await["on_order"]), 2500.0);

    // Changing the factor later does not change what the order says.
    let (_, units) = t
        .get(
            &t.admin,
            &format!("/api/entities/item_uom?item_id={}", s.steel),
        )
        .await;
    let tonne = units["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/item_uom/{tonne}"),
            json!({ "factor": "907.18474" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 1.5 t arrive: 1,500 kg worth 1,800.00.
    let (status, receipt) = s
        .draft(
            &t,
            "purchase_receipt",
            json!({ "purchase_order_id": order["id"], "lines": [
                { "order_line_id": order["body"]["lines"][0]["id"], "qty": "1.5" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    s.ok(&t, "purchase_receipt.post", json!({ "id": id(&receipt) }))
        .await;
    assert_eq!(s.on_hand(&t, &s.steel).await, (1500.0, 1800.0));
    let (_, order) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_order/{}", id(&order)),
        )
        .await;
    assert_eq!(dec(&order["body"]["lines"][0]["qty_received"]), 1.5);
    assert_eq!(dec(&s.availability(&t, &s.steel).await["on_order"]), 1000.0);

    // Half a tonne goes back: 500 kg.
    let back = s
        .ok(&t, "purchase_order.return", json!({ "id": id(&order) }))
        .await;
    let mut body = back["body"].clone();
    body["lines"][0]["qty"] = json!("0.5");
    let (status, back) = t
        .patch(
            &t.admin,
            &format!("/api/documents/purchase_return/{}", id(&back)),
            json!({ "version": back["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    s.ok(&t, "purchase_return.post", json!({ "id": id(&back) }))
        .await;
    assert_eq!(s.on_hand(&t, &s.steel).await, (1000.0, 1200.0));

    // The printed order says tonnes.
    let printed = s
        .ok(
            &t,
            "document.render",
            json!({ "doc_type": "purchase_order", "id": id(&order) }),
        )
        .await;
    assert!(printed["html"].as_str().unwrap().contains("<td>t</td>"));
    t.stop().await;
}

#[tokio::test]
async fn bolts_are_sold_by_the_box_and_leave_one_by_one() {
    let t = TestApp::start().await;
    let s = shop(&t).await;
    // 1,000 bolts in stock at 0.10.
    let (status, opening) = s
        .draft(
            &t,
            "purchase_order",
            json!({ "vendor_id": s.vendor, "lines": [
                { "item_id": s.bolt, "warehouse_id": s.plant, "qty": "1000",
                  "unit_price": "0.10" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{opening}");
    let opening = s
        .ok(&t, "purchase_order.approve", json!({ "id": id(&opening) }))
        .await;
    let (_, receipt) = s
        .draft(
            &t,
            "purchase_receipt",
            json!({ "purchase_order_id": opening["id"], "lines": [
                { "order_line_id": opening["body"]["lines"][0]["id"], "qty": "1000" }] }),
        )
        .await;
    s.ok(&t, "purchase_receipt.post", json!({ "id": id(&receipt) }))
        .await;

    // An eighth of a bolt in a kit: 0.333333 kits would be 0.041666625
    // bolts, more places than stock is kept in: refused, never rounded.
    t.create(
        "item_uom",
        json!({ "item_id": s.bolt, "uom": "kit", "factor": "0.125" }),
    )
    .await;
    let (status, body) = s
        .draft(
            &t,
            "sales_order",
            json!({ "customer_id": s.customer, "lines": [
                { "item_id": s.bolt, "warehouse_id": s.plant, "qty": "0.333333", "uom": "kit",
                  "unit_price": "20" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // 3 boxes at 20.00 a box: 300 bolts reserved.
    let (status, order) = s
        .draft(
            &t,
            "sales_order",
            json!({ "customer_id": s.customer, "lines": [
                { "item_id": s.bolt, "warehouse_id": s.plant, "qty": "3", "uom": "box",
                  "unit_price": "20" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{order}");
    assert_eq!(dec(&order["summary"]["total_net"]), 60.0, "{order}");
    let order = s
        .ok(&t, "sales_order.approve", json!({ "id": id(&order) }))
        .await;
    let row = s.availability(&t, &s.bolt).await;
    assert_eq!(dec(&row["committed"]), 300.0, "{row}");
    assert_eq!(dec(&row["reserved"]), 300.0, "{row}");

    // Two boxes go: 200 bolts at 0.10 leave.
    let delivery = s
        .ok(&t, "sales_order.deliver", json!({ "id": id(&order) }))
        .await;
    let mut body = delivery["body"].clone();
    body["lines"][0]["qty"] = json!("2");
    let (status, delivery) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_delivery/{}", id(&delivery)),
            json!({ "version": delivery["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    let posted = s
        .ok(&t, "sales_delivery.post", json!({ "id": id(&delivery) }))
        .await;
    assert_eq!(dec(&posted["body"]["lines"][0]["value"]), 20.0, "{posted}");
    assert_eq!(s.on_hand(&t, &s.bolt).await, (800.0, 80.0));
    let row = s.availability(&t, &s.bolt).await;
    assert_eq!(dec(&row["committed"]), 100.0, "{row}");
    assert_eq!(dec(&row["reserved"]), 100.0, "{row}");

    // Invoiced by the box, and printed by the box.
    let invoice = s
        .ok(&t, "sales_order.invoice", json!({ "id": id(&order) }))
        .await;
    assert_eq!(dec(&invoice["body"]["lines"][0]["qty"]), 2.0, "{invoice}");
    assert_eq!(dec(&invoice["summary"]["total_net"]), 40.0, "{invoice}");
    let printed = s
        .ok(
            &t,
            "document.render",
            json!({ "doc_type": "sales_delivery", "id": id(&delivery) }),
        )
        .await;
    assert!(printed["html"].as_str().unwrap().contains("<td>box</td>"));

    // One box comes back: 100 bolts, at the cost they left for.
    let back = s
        .ok(&t, "sales_order.return", json!({ "id": id(&order) }))
        .await;
    let mut body = back["body"].clone();
    body["lines"][0]["qty"] = json!("1");
    let (status, back) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_return/{}", id(&back)),
            json!({ "version": back["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    s.ok(&t, "sales_return.post", json!({ "id": id(&back) }))
        .await;
    assert_eq!(s.on_hand(&t, &s.bolt).await, (900.0, 90.0));
    t.stop().await;
}

#[tokio::test]
async fn a_price_list_kept_per_stock_unit_prices_a_box() {
    let t = TestApp::start().await;
    let s = shop(&t).await;
    // Bolts at 0.25 each, 0.20 from 500.
    let list = t
        .create(
            "price_list",
            json!({ "company_id": s.c, "code": "BASE", "name": "Base", "side": "sales",
                    "currency": "USD", "valid_from": null }),
        )
        .await;
    for (min, price) in [("0", "0.25"), ("500", "0.20")] {
        t.create(
            "price_list_line",
            json!({ "price_list_id": list, "item_id": s.bolt, "min_qty": min,
                    "unit_price": price }),
        )
        .await;
    }
    let price_of = |qty: &'static str| {
        let t = &t;
        let s = &s;
        async move {
            let (status, order) = s
                .draft(
                    t,
                    "sales_order",
                    json!({ "customer_id": s.customer, "lines": [
                        { "item_id": s.bolt, "warehouse_id": s.plant, "qty": qty,
                          "uom": "box" }] }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{order}");
            dec(&order["body"]["lines"][0]["unit_price"])
        }
    };
    // 2 boxes are 200 bolts: 25.00 a box; 5 boxes are 500: 20.00 a box.
    assert_eq!(price_of("2").await, 25.0);
    assert_eq!(price_of("5").await, 20.0);
    t.stop().await;
}
