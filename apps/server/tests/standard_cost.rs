//! Standard costing beside moving average, in one company: an item moved
//! to standard with stock on hand (revalued), bought at a price variance,
//! made at standard with material and rate variances, sold at standard,
//! a landed cost that is a variance, a new standard revaluing stock; and
//! every ledger agreeing at the end.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct Works {
    c: String,
    plant: String,
    depot: String,
    /// Each site and its value of the SITE dimension.
    sites: Vec<(String, String)>,
    vendor: String,
    customer: String,
    steel: String,
    bolt: String,
    widget: String,
    bom: String,
    labour: String,
    revaluation: String,
    material_variance: String,
    rate_variance: String,
    price_variance: String,
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
    let account = |code: &'static str, kind: &'static str, purpose: &'static str| {
        let c = c.clone();
        async move {
            let account = t
                .create(
                    "account",
                    json!({ "company_id": c, "code": code, "name": purpose, "account_type": kind }),
                )
                .await;
            t.create(
                "account_rule",
                json!({ "company_id": c, "purpose": purpose, "account_id": account }),
            )
            .await;
            account
        }
    };
    let revaluation = account("5071", "expense", "standard_cost_revaluation").await;
    let material_variance = account("5072", "expense", "material_variance").await;
    let rate_variance = account("5073", "expense", "production_rate_variance").await;
    let price_variance: String = sqlx::query_scalar(
        "SELECT price_variance_account_id::text FROM purchasing_settings WHERE company_id = $1::uuid",
    )
    .bind(&c)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    // Two sites, each with its value of the SITE dimension.
    let dimension = t
        .create(
            "dimension",
            json!({ "company_id": c, "code": "SITE", "name": "Site" }),
        )
        .await;
    let mut sites = vec![];
    for code in ["OH", "TX"] {
        let site = t
            .create(
                "site",
                json!({ "company_id": c, "code": code, "name": code }),
            )
            .await;
        let value = t
            .create(
                "dimension_value",
                json!({ "company_id": c, "dimension_id": dimension, "code": code, "name": code }),
            )
            .await;
        t.create(
            "dimension_rule",
            json!({ "company_id": c, "scope_type": "site", "scope_id": site, "value_id": value }),
        )
        .await;
        sites.push((site, value));
    }
    let plant = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "PLANT", "name": "Plant", "site_id": sites[0].0 }),
        )
        .await;
    let depot = t
        .create(
            "warehouse",
            json!({ "company_id": c, "code": "DEPOT", "name": "Depot", "site_id": sites[1].0 }),
        )
        .await;
    t.create(
        "policy_rule",
        json!({ "company_id": c, "point": "stock_transfer.between_sites", "effect": "allow",
                "condition": null }),
    )
    .await;
    let vendor = t
        .create("vendor", json!({ "code": "MILL", "name": "Steel Mill" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "SHOP", "name": "Shop" }))
        .await;
    let item = |code: &'static str| async move {
        t.create("item", json!({ "code": code, "name": code }))
            .await
    };
    let steel = item("STEEL").await;
    let bolt = item("BOLT").await;
    let widget = item("WIDGET").await;
    // A widget: 2 steel and 4 bolts, and half an hour of labour.
    let bom = t
        .create(
            "bom",
            json!({ "item_id": widget, "code": "W-1", "name": "Widget", "output_qty": "1" }),
        )
        .await;
    for (component, qty) in [(&steel, "2"), (&bolt, "4")] {
        t.create(
            "bom_line",
            json!({ "bom_id": bom, "item_id": component, "qty": qty }),
        )
        .await;
    }
    let labour = t
        .create(
            "cost_activity",
            json!({ "code": "ASSY", "name": "Assembly", "kind": "labor" }),
        )
        .await;
    t.create(
        "cost_activity_rate",
        json!({ "company_id": c, "activity_id": labour, "rate": "40" }),
    )
    .await;
    t.create(
        "bom_activity",
        json!({ "bom_id": bom, "activity_id": labour, "qty": "0.5" }),
    )
    .await;
    Works {
        c,
        plant,
        depot,
        sites,
        vendor,
        customer,
        steel,
        bolt,
        widget,
        bom,
        labour,
        revaluation,
        material_variance,
        rate_variance,
        price_variance,
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

    /// Bought and received: `qty` at `price`. Returns the receipt.
    async fn buy(&self, t: &TestApp, item: &str, qty: &str, price: &str, date: &str) -> Value {
        let order = self
            .draft(
                t,
                "purchase_order",
                date,
                json!({ "vendor_id": self.vendor, "lines": [
                    { "item_id": item, "warehouse_id": self.plant, "qty": qty,
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

    async fn level(&self, t: &TestApp, item: &str) -> (f64, f64) {
        let body = self
            .ok(
                t,
                "stock.on_hand",
                json!({ "company_id": self.c, "item_id": item }),
            )
            .await;
        // In all its warehouses.
        body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .fold((0.0, 0.0), |(q, v), r| {
                (q + dec(&r["qty"]), v + dec(&r["value"]))
            })
    }

    async fn balance(&self, t: &TestApp, account: &str) -> f64 {
        let tb = self
            .ok(t, "gl.trial_balance", json!({ "company_id": self.c }))
            .await;
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["account_id"] == json!(account))
            .map(|r| dec(&r["debit"]) - dec(&r["credit"]))
            .sum()
    }

    /// At each site, the inventory accounts equal the stock's value there.
    async fn sites_agree(&self, t: &TestApp) {
        let inventory: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT account_id::text FROM account_rule
             WHERE company_id = $1::uuid AND purpose IN ('inventory', 'inventory_in_transit')",
        )
        .bind(&self.c)
        .fetch_all(&t.pool)
        .await
        .unwrap();
        for (site, value) in &self.sites {
            let tb = self
                .ok(
                    t,
                    "gl.trial_balance",
                    json!({ "company_id": self.c, "dimension_value_ids": [value] }),
                )
                .await;
            let ledger: f64 = tb["rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| inventory.contains(&r["account_id"].as_str().unwrap().to_owned()))
                .map(|r| dec(&r["debit"]) - dec(&r["credit"]))
                .sum();
            let stock = self
                .ok(
                    t,
                    "stock.on_hand",
                    json!({ "company_id": self.c, "site_id": site }),
                )
                .await;
            assert!(
                (ledger - dec(&stock["total_value"])).abs() < 0.005,
                "site {site}: ledger {ledger}, stock {stock}"
            );
        }
    }

    async fn version(&self, t: &TestApp, date: &str, lines: Value) -> Value {
        self.draft(t, "cost_version", date, json!({ "lines": lines }))
            .await
    }
}

fn message(body: &Value) -> String {
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn standard_and_average_items_live_side_by_side_and_the_books_agree() {
    let t = TestApp::start().await;
    let w = works(&t).await;

    // Steel bought at average: 100 at 2.00. Bolts stay at average.
    w.buy(&t, &w.steel, "100", "2.00", "2026-03-02").await;
    w.buy(&t, &w.bolt, "400", "0.10", "2026-03-02").await;
    assert_eq!(w.level(&t, &w.steel).await, (100.0, 200.0));

    // Steel cannot go to standard by itself while it has stock.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/item_costing",
            json!({ "company_id": w.c, "item_id": w.steel, "method": "standard" }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED, "{body}");

    // A version: steel at 2.50 (switched), widgets rolled up and kept at
    // standard (no stock yet, so switched freely).
    t.create(
        "item_costing",
        json!({ "company_id": w.c, "item_id": w.widget, "method": "standard" }),
    )
    .await;
    let v1 = w
        .version(
            &t,
            "2026-03-05",
            json!([{ "item_id": w.steel, "unit_cost": "2.50", "material_cost": "2.50",
                     "switch_to_standard": true }]),
        )
        .await;
    let rolled = w
        .ok(
            &t,
            "cost_version.rollup",
            json!({ "id": id(&v1), "item_ids": [w.widget] }),
        )
        .await;
    let widget_line = rolled["version"]["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["item_id"] == json!(w.widget))
        .unwrap()
        .clone();
    // 2 x 2.50 steel + 4 x 0.10 bolts = 5.40 material, 0.5 h x 40 = 20 labour.
    assert_eq!(dec(&widget_line["material_cost"]), 5.4, "{rolled}");
    assert_eq!(dec(&widget_line["labour_cost"]), 20.0, "{rolled}");
    assert_eq!(dec(&widget_line["unit_cost"]), 25.4, "{rolled}");
    w.ok(&t, "cost_version.activate", json!({ "id": id(&v1) }))
        .await;
    // The 100 steel revalued from 200.00 to 250.00.
    assert_eq!(w.level(&t, &w.steel).await, (100.0, 250.0));
    assert_eq!(w.balance(&t, &w.revaluation).await, -50.0);

    // Bought at 2.40: in at 2.50, the 5.00 difference is price variance.
    let ppv_before = w.balance(&t, &w.price_variance).await;
    w.buy(&t, &w.steel, "50", "2.40", "2026-03-06").await;
    assert_eq!(w.level(&t, &w.steel).await, (150.0, 375.0));
    assert_eq!(w.balance(&t, &w.price_variance).await - ppv_before, -5.0);

    // Bolts got dearer: 400 more at 0.20, average 0.15. Labour now 44 an
    // hour. Ten widgets: material 20 x 2.50 + 40 x 0.15 = 56.00 against a
    // standard of 54.00; labour 5 h x 44 = 220 against 200.
    w.buy(&t, &w.bolt, "400", "0.20", "2026-03-07").await;
    let (_, rates) = t
        .get(
            &t.admin,
            &format!("/api/entities/cost_activity_rate?activity_id={}", w.labour),
        )
        .await;
    let rate = rates["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/cost_activity_rate/{rate}"),
            json!({ "rate": "44" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let order = w
        .draft(
            &t,
            "production_order",
            "2026-03-08",
            json!({ "bom_id": w.bom, "qty": "10", "warehouse_id": w.plant }),
        )
        .await;
    w.ok(&t, "production_order.release", json!({ "id": id(&order) }))
        .await;
    w.ok(&t, "production_order.complete", json!({ "id": id(&order) }))
        .await;
    assert_eq!(w.level(&t, &w.widget).await, (10.0, 254.0));
    assert_eq!(w.balance(&t, &w.material_variance).await, 2.0);
    assert_eq!(w.balance(&t, &w.rate_variance).await, 20.0);

    // Three widgets sold leave at standard: 76.20.
    let sale = w
        .draft(
            &t,
            "sales_order",
            "2026-03-09",
            json!({ "customer_id": w.customer, "lines": [
                { "item_id": w.widget, "warehouse_id": w.plant, "qty": "3",
                  "unit_price": "40" }] }),
        )
        .await;
    w.ok(&t, "sales_order.approve", json!({ "id": id(&sale) }))
        .await;
    let delivery = w
        .ok(&t, "sales_order.deliver", json!({ "id": id(&sale) }))
        .await;
    let delivered = w
        .ok(&t, "sales_delivery.post", json!({ "id": id(&delivery) }))
        .await;
    assert_eq!(
        dec(&delivered["body"]["lines"][0]["value"]),
        76.2,
        "{delivered}"
    );

    // Freight on the steel bought on the 6th: not stock value at standard.
    let receipt = w.buy(&t, &w.steel, "10", "2.50", "2026-03-10").await;
    let freight = t
        .create(
            "charge",
            json!({ "code": "FREIGHT", "name": "Freight", "kind": "freight", "landed_cost": true }),
        )
        .await;
    let ppv_before = w.balance(&t, &w.price_variance).await;
    let bill = w
        .draft(
            &t,
            "vendor_invoice",
            "2026-03-11",
            json!({ "vendor_id": w.vendor, "lines": [
                { "charge_id": freight, "qty": "1", "unit_price": "12.00",
                  "receipt_ids": [receipt["id"]] }] }),
        )
        .await;
    w.ok(&t, "vendor_invoice.post", json!({ "id": id(&bill) }))
        .await;
    assert_eq!(w.level(&t, &w.steel).await, (140.0, 350.0));
    assert_eq!(w.balance(&t, &w.price_variance).await - ppv_before, 12.0);

    // 40 of the steel go to the depot in Texas, at standard.
    let transfer = w
        .draft(
            &t,
            "stock_transfer",
            "2026-03-12",
            json!({ "from_warehouse_id": w.plant, "to_warehouse_id": w.depot,
                    "lines": [{ "item_id": w.steel, "qty": "40" }] }),
        )
        .await;
    w.ok(&t, "stock_transfer.post", json!({ "id": id(&transfer) }))
        .await;
    w.sites_agree(&t).await;

    // A new standard for steel: 140 on hand revalued from 2.50 to 2.60,
    // each site's share at that site.
    let v2 = w
        .version(
            &t,
            "2026-03-20",
            json!([{ "item_id": w.steel, "unit_cost": "2.60", "material_cost": "2.60" }]),
        )
        .await;
    w.ok(&t, "cost_version.activate", json!({ "id": id(&v2) }))
        .await;
    let (_, at_depot) = t
        .command(
            &t.admin,
            "stock.on_hand",
            json!({ "company_id": w.c, "item_id": w.steel, "warehouse_id": w.depot }),
        )
        .await;
    assert_eq!(dec(&at_depot["total_value"]), 104.0, "{at_depot}");
    assert_eq!(w.balance(&t, &w.revaluation).await, -64.0);
    w.sites_agree(&t).await;

    // A standard is written only by activating a version; a standard item's
    // value does not take a cost adjustment.
    let refused = sqlx::query("UPDATE item_standard SET unit_cost = 1 WHERE item_id = $1::uuid")
        .bind(&w.steel)
        .execute(&t.pool)
        .await;
    assert!(refused.is_err());

    // Back to average is allowed at any time; steel's value carries on.
    let (_, costing) = t
        .get(
            &t.admin,
            &format!("/api/entities/item_costing?item_id={}", w.steel),
        )
        .await;
    let steel_costing = costing["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/item_costing/{steel_costing}"),
            json!({ "method": "average" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(w.level(&t, &w.steel).await, (140.0, 364.0));

    // Every ledger agrees.
    w.sites_agree(&t).await;
    let report = erpcraft_server::verify::verify(&t.pool, Some("WORKS"))
        .await
        .unwrap();
    assert!(report.ok, "{}", report.text());
    t.stop().await;
}

#[tokio::test]
async fn a_bill_that_leads_back_to_itself_is_refused_and_a_standard_item_needs_a_standard() {
    let t = TestApp::start().await;
    let w = works(&t).await;
    // Bolts are made from widgets now: widget -> bolt -> widget.
    let loop_bom = t
        .create(
            "bom",
            json!({ "item_id": w.bolt, "code": "B-1", "name": "Bolt", "output_qty": "1" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": loop_bom, "item_id": w.widget, "qty": "1" }),
    )
    .await;
    let v = w
        .version(
            &t,
            "2026-03-05",
            json!([{ "item_id": w.widget, "unit_cost": "1" }]),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "cost_version.rollup", json!({ "id": id(&v) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("lead back"), "{body}");
    assert!(
        message(&body).contains("WIDGET -> BOLT -> WIDGET"),
        "{body}"
    );

    // Steel at standard with no standard yet: it cannot move.
    t.create(
        "item_costing",
        json!({ "company_id": w.c, "item_id": w.steel, "method": "standard" }),
    )
    .await;
    let order = w
        .draft(
            &t,
            "purchase_order",
            "2026-03-02",
            json!({ "vendor_id": w.vendor, "lines": [
                { "item_id": w.steel, "warehouse_id": w.plant, "qty": "1",
                  "unit_price": "2" }] }),
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
                { "order_line_id": order["body"]["lines"][0]["id"], "qty": "1" }] }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_receipt.post",
            json!({ "id": id(&receipt) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("no standard"), "{body}");
    t.stop().await;
}
