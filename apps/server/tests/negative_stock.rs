//! Stock below zero, where a company's policy allows it: what the missing
//! goods leave at, what happens when they arrive, and that the inventory
//! account equals the value of the stock at every step.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

const NEGATIVE: &str = "stock.negative";

struct Fixture {
    c: Company,
    widget: String,
    main: String,
    vendor: String,
    customer: String,
    /// Stock adjustments are posted against this account.
    offset: String,
    /// The account for stock variances; not yet named in an account rule.
    variance: String,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let widget = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let main = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyers Ltd" }))
        .await;
    let account = |code: &'static str, name: &'static str, kind: &'static str| {
        let company = c.id.clone();
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
            )
            .await
        }
    };
    let receivable = account("1200", "Accounts receivable", "asset").await;
    let inventory = account("1300", "Inventory", "asset").await;
    let payable = account("2000", "Accounts payable", "liability").await;
    let grni = account("2100", "Goods received not invoiced", "liability").await;
    let cogs = account("5000", "Cost of goods sold", "expense").await;
    let offset = account("5100", "Stock adjustments", "expense").await;
    let price_variance = account("5200", "Purchase price variance", "expense").await;
    let variance = account("5300", "Stock variance", "expense").await;
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id, "grni_account_id": grni, "payable_account_id": payable,
            "price_variance_account_id": price_variance,
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id, "receivable_account_id": receivable,
            "revenue_account_id": c.revenue, "cogs_account_id": cogs,
        }),
    )
    .await;
    t.inventory_rule(&c.id, &inventory).await;
    Fixture {
        c,
        widget,
        main,
        vendor,
        customer,
        offset,
        variance,
    }
}

/// Save a policy rule for stock below zero; returns its id.
async fn allow(t: &TestApp, f: &Fixture, condition: Value) -> String {
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": NEGATIVE, "effect": "allow", "condition": condition }),
    )
    .await
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

impl Fixture {
    /// Say which account stock variances go to.
    async fn variance_rule(&self, t: &TestApp) {
        t.create(
            "account_rule",
            json!({ "company_id": self.c.id, "purpose": "stock_variance", "account_id": self.variance }),
        )
        .await;
    }

    /// Draft a document and run `command` on it.
    async fn posted(
        &self,
        t: &TestApp,
        doc_type: &str,
        command: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, doc) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc}");
        t.command(&t.admin, command, json!({ "id": id(&doc) }))
            .await
    }

    /// A stock adjustment of one item in one warehouse.
    async fn adjust_in(
        &self,
        t: &TestApp,
        item: &str,
        warehouse: &str,
        qty: &str,
        unit_cost: Option<&str>,
    ) -> (StatusCode, Value) {
        let body = json!({
            "offset_account_id": self.offset,
            "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": qty, "unit_cost": unit_cost }],
        });
        self.posted(t, "stock_adjustment", "stock_adjustment.post", body)
            .await
    }

    /// A stock adjustment of widgets in the main warehouse, which must post.
    async fn adjust(&self, t: &TestApp, qty: &str, unit_cost: Option<&str>) -> Value {
        let (status, body) = self
            .adjust_in(t, &self.widget, &self.main, qty, unit_cost)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// An approved order of `doc_type` for `qty` widgets at `price`.
    async fn order(&self, t: &TestApp, doc_type: &str, qty: &str, price: &str) -> Value {
        let mut body = json!({
            "lines": [{ "item_id": self.widget, "warehouse_id": self.main, "qty": qty, "unit_price": price }],
        });
        match doc_type {
            "purchase_order" => body["vendor_id"] = json!(self.vendor),
            _ => body["customer_id"] = json!(self.customer),
        }
        let (status, order) = self
            .posted(t, doc_type, &format!("{doc_type}.approve"), body)
            .await;
        assert_eq!(status, StatusCode::OK, "{order}");
        order
    }

    /// Order `qty` widgets from the customer and deliver them at once.
    async fn deliver(&self, t: &TestApp, qty: &str) -> (StatusCode, Value) {
        let order = self.order(t, "sales_order", qty, "9.00").await;
        let body = json!({
            "sales_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
        });
        self.posted(t, "sales_delivery", "sales_delivery.post", body)
            .await
    }

    /// Order `qty` widgets from the vendor at `price` and receive them.
    async fn receive(&self, t: &TestApp, qty: &str, price: &str) -> (StatusCode, Value) {
        let order = self.order(t, "purchase_order", qty, price).await;
        let body = json!({
            "purchase_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
        });
        self.posted(t, "purchase_receipt", "purchase_receipt.post", body)
            .await
    }

    async fn reverse(&self, t: &TestApp, doc_type: &str, doc: &Value) -> (StatusCode, Value) {
        t.command(
            &t.admin,
            &format!("{doc_type}.reverse"),
            json!({ "id": id(doc) }),
        )
        .await
    }

    /// The balance of an account. On the way: the books balance, and the
    /// inventory account equals the value of the stock, below zero or not.
    async fn balance(&self, t: &TestApp, code: &str) -> f64 {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        let balance = |code: &str| {
            tb["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["code"] == code)
                .map_or(0.0, |r| dec(&r["balance"]))
        };
        let (_, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(
            dec(&on_hand["total_value"]),
            balance("1300"),
            "stock value and the inventory account agree"
        );
        balance(code)
    }

    /// Quantity and value of an item in a warehouse as `stock.on_hand`
    /// reports them, after the checks of [`Fixture::balance`].
    async fn level_of(&self, t: &TestApp, item: &str, warehouse: &str) -> (f64, f64) {
        self.balance(t, "1300").await;
        let (_, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id, "item_id": item, "warehouse_id": warehouse }),
            )
            .await;
        match on_hand["rows"].as_array().unwrap().first() {
            Some(row) => (dec(&row["qty"]), dec(&row["value"])),
            None => (0.0, 0.0),
        }
    }

    /// Widgets in the main warehouse: quantity and value.
    async fn level(&self, t: &TestApp) -> (f64, f64) {
        self.level_of(t, &self.widget, &self.main).await
    }

    /// The last stock entry recorded.
    async fn last_entry(&self, t: &TestApp) -> Value {
        let (status, body) = t
            .get(
                &t.admin,
                "/api/entities/stock_entry?sort=-created_at,-line_no&limit=1",
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["items"][0].clone()
    }
}

#[tokio::test]
async fn by_default_stock_never_goes_below_zero() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, "4", Some("3.00")).await;

    let (status, body) = f.deliver(&t, "10").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "only 4 each of item WIDGET are on hand in warehouse MAIN"
    );
    let (status, body) = f.adjust_in(&t, &f.widget, &f.main, "-5", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "only 4 each of item WIDGET are on hand in warehouse MAIN"
    );
    assert_eq!(f.level(&t).await, (4.0, 12.0));

    // Within what is on hand nothing is asked and nothing is marked.
    let (status, body) = f.deliver(&t, "4").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = f.last_entry(&t).await;
    assert_eq!(entry["negative_rule_id"], Value::Null);
    assert_eq!(dec(&entry["variance"]), 0.0);
    assert_eq!(f.level(&t).await, (0.0, 0.0));
    t.stop().await;
}

#[tokio::test]
async fn goods_leave_before_they_arrive_and_are_caught_up() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let rule = allow(&t, &f, Value::Null).await;

    // Four on hand, bought at 3.00.
    let (status, first) = f.receive(&t, "4", "3.00").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(f.level(&t).await, (4.0, 12.0));

    // Ten are delivered. The four on hand leave with their 12.00; the six
    // that are not leave at the last cost, 3.00 each: 30.00 in all.
    let (status, delivery) = f.deliver(&t, "10").await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    assert_eq!(f.level(&t).await, (-6.0, -18.0));
    assert_eq!(f.balance(&t, "1300").await, -18.0);
    assert_eq!(f.balance(&t, "5000").await, 30.0);
    // The entry says that it went below zero, and by which rule.
    let entry = f.last_entry(&t).await;
    assert_eq!(dec(&entry["qty"]), -10.0);
    assert_eq!(dec(&entry["value"]), -30.0);
    assert_eq!(dec(&entry["qty_after"]), -6.0);
    assert_eq!(dec(&entry["value_after"]), -18.0);
    assert_eq!(entry["negative_rule_id"], json!(rule));
    // A rule that an entry names stays on record.
    let (status, body) = t
        .send(
            "DELETE",
            &format!("/api/entities/policy_rule/{rule}"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");

    // Ten arrive at 3.50. A variance needs an account to go to.
    let (status, body) = f.receive(&t, "10", "3.50").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("'stock_variance'"), "{body}");
    assert_eq!(f.level(&t).await, (-6.0, -18.0));
    f.variance_rule(&t).await;

    // Six of the ten cover what is missing: they cost 21.00 and the
    // shortfall was carried at 18.00, so 3.00 is variance. The other four
    // come into stock at their real cost, 14.00.
    let (status, receipt) = f.receive(&t, "10", "3.50").await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(f.level(&t).await, (4.0, 14.0));
    assert_eq!(f.balance(&t, "1300").await, 14.0);
    assert_eq!(f.balance(&t, "5300").await, 3.0);
    // All 35.00 is owed for, on top of the 12.00 of the first receipt.
    assert_eq!(f.balance(&t, "2100").await, -47.0);
    let entry = f.last_entry(&t).await;
    assert_eq!(dec(&entry["value"]), 32.0);
    assert_eq!(dec(&entry["variance"]), 3.0);
    assert_eq!(entry["negative_rule_id"], Value::Null);
    assert_eq!(dec(&receipt["body"]["lines"][0]["value"]), 35.0);

    // Taking the receipt back restores the level below zero exactly, and
    // takes the variance back with it.
    let (status, body) = f.reverse(&t, "purchase_receipt", &receipt).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-6.0, -18.0));
    assert_eq!(f.balance(&t, "5300").await, 0.0);
    assert_eq!(f.balance(&t, "2100").await, -12.0);
    let entry = f.last_entry(&t).await;
    assert_eq!(dec(&entry["value"]), -32.0);
    assert_eq!(dec(&entry["variance"]), -3.0);

    // Taking the delivery back puts everything where it was before it.
    let (status, body) = f.reverse(&t, "sales_delivery", &delivery).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (4.0, 12.0));
    assert_eq!(f.balance(&t, "5000").await, 0.0);
    t.stop().await;
}

#[tokio::test]
async fn a_receipt_is_not_taken_back_further_below_zero_than_it_found_the_level() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    allow(&t, &f, Value::Null).await;
    f.variance_rule(&t).await;
    f.adjust(&t, "4", Some("3.00")).await;
    let (status, delivery) = f.deliver(&t, "10").await;
    assert_eq!(status, StatusCode::OK, "{delivery}");
    let (status, receipt) = f.receive(&t, "10", "3.50").await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(f.level(&t).await, (4.0, 14.0));

    // Two more leave. Taking the receipt back would now leave eight
    // missing where six were: goods it brought in have moved on.
    let (status, body) = f.deliver(&t, "2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (2.0, 7.0));
    let (status, body) = f.reverse(&t, "purchase_receipt", &receipt).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "this would leave -8 each of item WIDGET in warehouse MAIN; the stock has already moved on"
    );

    // The delivery that went below zero can still be taken back after the
    // goods arrived: the ten return at the 30.00 they left for. The
    // variance stays: it was real.
    let (status, body) = f.reverse(&t, "sales_delivery", &delivery).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (12.0, 37.0));
    assert_eq!(f.balance(&t, "5300").await, 3.0);
    t.stop().await;
}

#[tokio::test]
async fn a_receipt_that_exactly_covers_what_is_missing_leaves_nothing() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    allow(&t, &f, Value::Null).await;
    f.variance_rule(&t).await;
    f.adjust(&t, "4", Some("3.00")).await;
    let (status, body) = f.deliver(&t, "10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-6.0, -18.0));

    // Two arrive at 4.00: they cover a third of what is missing, carried
    // at 6.00, so 2.00 is variance and none of it becomes stock.
    f.adjust(&t, "2", Some("4.00")).await;
    assert_eq!(f.level(&t).await, (-4.0, -12.0));
    assert_eq!(f.balance(&t, "5300").await, 2.0);
    let entry = f.last_entry(&t).await;
    assert_eq!(dec(&entry["value"]), 6.0);
    assert_eq!(dec(&entry["variance"]), 2.0);
    // The adjustment's own account took the whole 8.00.
    assert_eq!(f.balance(&t, "5100").await, -12.0 - 8.0);

    // While below zero, more leaves at the last cost, now 4.00.
    let (status, body) = f.deliver(&t, "1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-5.0, -16.0));

    // Five arrive at 2.00 and cover exactly what is missing. The shortfall
    // was carried at 16.00 and cost 10.00: 6.00 less than estimated. The
    // shelf is empty and worth nothing.
    f.adjust(&t, "5", Some("2.00")).await;
    assert_eq!(f.level(&t).await, (0.0, 0.0));
    assert_eq!(f.balance(&t, "1300").await, 0.0);
    assert_eq!(f.balance(&t, "5300").await, 2.0 - 6.0);
    let entry = f.last_entry(&t).await;
    assert_eq!(dec(&entry["value"]), 16.0);
    assert_eq!(dec(&entry["variance"]), -6.0);
    assert_eq!(dec(&entry["qty_after"]), 0.0);
    assert_eq!(dec(&entry["value_after"]), 0.0);
    let (_, on_hand) = t
        .command(&t.admin, "stock.on_hand", json!({ "company_id": f.c.id }))
        .await;
    assert_eq!(on_hand["rows"], json!([]));
    t.stop().await;
}

#[tokio::test]
async fn a_rule_can_allow_it_for_one_kind_of_movement_only() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.variance_rule(&t).await;
    allow(&t, &f, json!("facts.context == 'sales_delivery'")).await;
    t.create(
        "policy_rule",
        json!({
            "company_id": f.c.id, "point": NEGATIVE, "effect": "refuse",
            "condition": "facts.context == 'adjustment'", "priority": 1,
            "message": "a stock count may not take {item_code} below zero in {warehouse_code}: {on_hand} on hand, {issuing} to take out, short by {short_by}",
        }),
    )
    .await;
    let dock = t
        .create(
            "warehouse",
            json!({ "company_id": f.c.id, "code": "DOCK", "name": "Loading dock" }),
        )
        .await;
    f.adjust(&t, "4", Some("3.00")).await;

    // An adjustment is refused in the company's words, a transfer by the
    // default in the ledger's.
    let (status, body) = f.adjust_in(&t, &f.widget, &f.main, "-5", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "a stock count may not take WIDGET below zero in MAIN: 4 on hand, 5 to take out, short by 1"
    );
    let transfer = |qty: &'static str| {
        let (t, f, dock) = (&t, &f, dock.clone());
        async move {
            let body = json!({
                "from_warehouse_id": f.main, "to_warehouse_id": dock,
                "lines": [{ "item_id": f.widget, "qty": qty }],
            });
            f.posted(t, "stock_transfer", "stock_transfer.post", body)
                .await
        }
    };
    let (status, body) = transfer("5").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "only 4 each of item WIDGET are on hand in warehouse MAIN"
    );
    assert_eq!(f.level(&t).await, (4.0, 12.0));

    // A delivery is allowed, also one that takes a level already below
    // zero further down.
    let (status, body) = f.deliver(&t, "5").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-1.0, -3.0));
    let (status, body) = f.deliver(&t, "2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-3.0, -9.0));
    t.stop().await;
}

#[tokio::test]
async fn a_transfer_into_a_warehouse_below_zero_posts_the_variance() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.variance_rule(&t).await;
    allow(&t, &f, json!("facts.warehouse_code == 'DOCK'")).await;
    let dock = t
        .create(
            "warehouse",
            json!({ "company_id": f.c.id, "code": "DOCK", "name": "Loading dock" }),
        )
        .await;
    f.adjust(&t, "10", Some("2.00")).await;

    // Three leave the dock, where nothing was ever received: no cost is
    // known there, so they leave at nothing.
    let (status, body) = f.adjust_in(&t, &f.widget, &dock, "-3", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level_of(&t, &f.widget, &dock).await, (-3.0, 0.0));
    assert_eq!(f.balance(&t, "1300").await, 20.0);

    // Five are moved to the dock, worth 10.00. Three cover what is
    // missing: their 6.00 is all variance. Two are on hand at 4.00.
    let body = json!({
        "from_warehouse_id": f.main, "to_warehouse_id": dock,
        "lines": [{ "item_id": f.widget, "qty": "5" }],
    });
    let (status, transfer) = f
        .posted(&t, "stock_transfer", "stock_transfer.post", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{transfer}");
    assert_eq!(f.level(&t).await, (5.0, 10.0));
    assert_eq!(f.level_of(&t, &f.widget, &dock).await, (2.0, 4.0));
    assert_eq!(f.balance(&t, "1300").await, 14.0);
    assert_eq!(f.balance(&t, "5300").await, 6.0);

    let (status, body) = f.reverse(&t, "stock_transfer", &transfer).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (10.0, 20.0));
    assert_eq!(f.level_of(&t, &f.widget, &dock).await, (-3.0, 0.0));
    assert_eq!(f.balance(&t, "5300").await, 0.0);
    t.stop().await;
}

#[tokio::test]
async fn production_completes_with_a_component_short_and_a_product_already_gone() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.variance_rule(&t).await;
    // The widget is made from two bolts.
    let bolt = t
        .create("item", json!({ "code": "BOLT", "name": "Bolt" }))
        .await;
    let bom = t
        .create(
            "bom",
            json!({ "item_id": f.widget, "code": "W-1", "name": "Widget", "output_qty": "1" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": bolt, "qty": "2" }),
    )
    .await;
    // Two bolts on hand at 1.00; three widgets need six.
    let (status, body) = f.adjust_in(&t, &bolt, &f.main, "2", Some("1.00")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, order) = f
        .posted(
            &t,
            "production_order",
            "production_order.release",
            json!({ "bom_id": bom, "qty": "3", "warehouse_id": f.main }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    let complete = || {
        let (t, order) = (&t, &order);
        async move {
            t.command(
                &t.admin,
                "production_order.complete",
                json!({ "id": id(order) }),
            )
            .await
        }
    };

    // By default a short component stops the order.
    let (status, body) = complete().await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "only 2 each of item BOLT are on hand in warehouse MAIN"
    );

    // The company lets deliveries and production run ahead of the stock.
    let rule = allow(
        &t,
        &f,
        json!("facts.context in ['sales_delivery', 'production']"),
    )
    .await;
    // Two widgets are delivered before any was made. No cost is known for
    // them yet, so they leave at nothing.
    let (status, body) = f.deliver(&t, "2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-2.0, 0.0));

    // The order completes. Six bolts are consumed, two on hand and four at
    // the last cost: 6.00. Three widgets are made for 6.00; two of them
    // cover what is missing, so their 4.00 is variance, and one is on
    // hand at 2.00.
    let (status, body) = complete().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level_of(&t, &bolt, &f.main).await, (-4.0, -4.0));
    assert_eq!(f.level(&t).await, (1.0, 2.0));
    assert_eq!(f.balance(&t, "1300").await, -2.0);
    assert_eq!(f.balance(&t, "5300").await, 4.0);
    let marked = t
        .count(&format!(
            "SELECT count(*) FROM stock_entry WHERE negative_rule_id = '{rule}' AND qty_after < 0"
        ))
        .await;
    assert_eq!(marked, 2, "the delivery and the component issue");

    // Taking the order back restores both levels and the variance.
    let (status, body) = t
        .command(
            &t.admin,
            "production_order.reverse",
            json!({ "id": id(&order) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level_of(&t, &bolt, &f.main).await, (2.0, 2.0));
    assert_eq!(f.level(&t).await, (-2.0, 0.0));
    assert_eq!(f.balance(&t, "5300").await, 0.0);
    t.stop().await;
}

#[tokio::test]
async fn the_ledger_itself_refuses_what_no_rule_allows() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, "4", Some("3.00")).await;
    // An entry written past the application, under an existing posting.
    let raw = |rule: &str| {
        format!(
            "INSERT INTO stock_entry (id, company_id, posting_id, line_no, item_id, warehouse_id,
                posting_date, qty, value, unit_cost, qty_after, value_after, negative_rule_id)
             SELECT gen_random_uuid(), company_id, posting_id, 99, item_id, warehouse_id,
                posting_date, -10, 0, 0, 0, 0, {rule} FROM stock_entry LIMIT 1"
        )
    };
    let refused = |sql: String| {
        let t = &t;
        async move {
            sqlx::query(&sql)
                .execute(&t.pool)
                .await
                .expect_err(&sql)
                .to_string()
        }
    };

    // No rule named: refused as ever.
    let why = refused(raw("NULL")).await;
    assert!(
        why.contains("only 4 each of item WIDGET are on hand"),
        "{why}"
    );

    // Naming something that is not a rule allowing this, for this company.
    let other_point = t
        .create(
            "policy_rule",
            json!({ "point": "purchase_receipt.over_receipt", "effect": "allow", "condition": null }),
        )
        .await;
    let refusing = t
        .create(
            "policy_rule",
            json!({ "point": NEGATIVE, "effect": "refuse", "condition": "facts.short_by > 100" }),
        )
        .await;
    let inactive = t
        .create(
            "policy_rule",
            json!({ "point": NEGATIVE, "effect": "allow", "condition": null, "active": false }),
        )
        .await;
    let elsewhere = t.company("BOLT", "USD").await;
    let other_company = t
        .create(
            "policy_rule",
            json!({ "company_id": elsewhere.id, "point": NEGATIVE, "effect": "allow", "condition": null }),
        )
        .await;
    for rule in [&other_point, &refusing, &inactive, &other_company] {
        let why = refused(raw(&format!("'{rule}'"))).await;
        assert!(
            why.contains("does not allow stock to go below zero in this company"),
            "{why}"
        );
    }
    let why = refused(raw("gen_random_uuid()")).await;
    assert!(!why.is_empty());

    // The level cannot be changed directly either, and holds no impossible
    // state.
    for sql in [
        "UPDATE stock_level SET qty = -5",
        "UPDATE stock_level SET last_cost = 100",
        "INSERT INTO stock_level (item_id, warehouse_id, company_id, qty, value)
         SELECT item_id, warehouse_id, company_id, -1, 5 FROM stock_level",
    ] {
        assert!(sqlx::query(sql).execute(&t.pool).await.is_err(), "{sql}");
    }
    assert_eq!(f.level(&t).await, (4.0, 12.0));
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 1);

    // The application is held to the same: with only another company's
    // rule in force, this company's stock stays above zero.
    let (status, body) = f.deliver(&t, "5").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    t.stop().await;
}

/// The last cost of widgets in the main warehouse, as the ledger keeps it.
async fn last_cost(t: &TestApp) -> Option<String> {
    sqlx::query_scalar("SELECT trim_scale(last_cost)::text FROM stock_level")
        .fetch_one(&t.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn taking_a_receipt_back_puts_back_the_cost_it_replaced() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.adjust(&t, "4", Some("3.00")).await;
    assert_eq!(last_cost(&t).await.as_deref(), Some("3"));
    let (status, first) = f.receive(&t, "2", "4.00").await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = f.receive(&t, "2", "5.00").await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(last_cost(&t).await.as_deref(), Some("5"));

    // The first receipt no longer gives the last cost: taking it back
    // changes nothing.
    let (status, body) = f.reverse(&t, "purchase_receipt", &first).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(last_cost(&t).await.as_deref(), Some("5"));
    // The second does. Taking it back falls back past the first, which
    // was taken back too, to the 3.00 of the adjustment.
    let (status, body) = f.reverse(&t, "purchase_receipt", &second).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(last_cost(&t).await.as_deref(), Some("3"));
    assert_eq!(f.level(&t).await, (4.0, 12.0));

    // So goods that are not on hand leave at 3.00, not at a cost that was
    // taken back: 12.00 + 6 × 3.00.
    allow(&t, &f, Value::Null).await;
    let (status, body) = f.deliver(&t, "10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.level(&t).await, (-6.0, -18.0));
    assert_eq!(f.balance(&t, "5000").await, 30.0);

    // Where nothing that stands was ever received, nothing is known.
    let t2 = TestApp::start().await;
    let g = setup(&t2).await;
    let (status, only) = g.receive(&t2, "2", "4.00").await;
    assert_eq!(status, StatusCode::OK, "{only}");
    let (status, body) = g.reverse(&t2, "purchase_receipt", &only).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(last_cost(&t2).await, None);
    t2.stop().await;
    t.stop().await;
}

#[tokio::test]
async fn an_entry_below_zero_keeps_the_facts_and_the_condition_it_was_allowed_on() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let rule = allow(&t, &f, json!("facts.short_by <= 8")).await;
    f.adjust(&t, "4", Some("3.00")).await;

    // Through the application: the facts the rule was asked with, and its
    // condition as it read then.
    let (status, body) = f.deliver(&t, "10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = f.last_entry(&t).await;
    assert_eq!(entry["negative_rule_id"], json!(rule));
    assert_eq!(entry["negative_condition"], "facts.short_by <= 8");
    assert_eq!(
        entry["negative_facts"],
        json!({
            "on_hand": "4", "issuing": "10", "short_by": "6",
            "item_code": "WIDGET", "warehouse_code": "MAIN", "context": "sales_delivery",
        })
    );
    // Changing the rule later does not change what the entry says.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{rule}"),
            json!({ "point": NEGATIVE, "condition": "facts.short_by <= 1" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = f.last_entry(&t).await;
    assert_eq!(entry["negative_condition"], "facts.short_by <= 8");

    // Past the application: the ledger cannot evaluate the condition, but
    // it holds the facts to the truth. Now 6 are missing; issuing 10 more
    // would leave 16 missing, which the rule does not allow.
    let raw = |line: i32, facts: &str| {
        format!(
            "INSERT INTO stock_entry (id, company_id, posting_id, line_no, item_id, warehouse_id,
                posting_date, qty, value, unit_cost, qty_after, value_after, negative_rule_id,
                negative_facts)
             SELECT gen_random_uuid(), company_id, posting_id, {line}, item_id, warehouse_id,
                posting_date, -10, 0, 0, 0, 0, '{rule}', {facts}
             FROM stock_entry ORDER BY seq LIMIT 1"
        )
    };
    let truth = r#"{"on_hand": "-6", "issuing": "10", "short_by": "16", "item_code": "WIDGET", "warehouse_code": "MAIN", "context": "adjustment"}"#;
    let lies = [
        "NULL".to_owned(),
        "'[]'".to_owned(),
        format!("'{}'", truth.replace("\"16\"", "\"2\"")),
        format!("'{}'", truth.replace("\"-6\"", "\"4\"")),
        format!(
            "'{}'",
            truth.replace("\"issuing\": \"10\"", "\"issuing\": \"1\"")
        ),
        format!("'{}'", truth.replace("WIDGET", "BOLT")),
        format!("'{}'", truth.replace("\"MAIN\"", "\"BACK\"")),
        format!("'{}'", truth.replace("\"adjustment\"", "\"\"")),
        format!("'{}'", truth.replace("\"16\"", "\"sixteen\"")),
        format!("'{}'", truth.replace(", \"context\": \"adjustment\"", "")),
    ];
    for (n, facts) in lies.iter().enumerate() {
        let why = sqlx::query(&raw(100 + n as i32, facts))
            .execute(&t.pool)
            .await
            .expect_err(facts)
            .to_string();
        assert!(
            why.contains("must carry the facts its policy rule was asked with"),
            "{facts}: {why}"
        );
    }
    // With the true facts it is let through, and whoever reads the entry
    // sees that the condition it names does not hold for them.
    sqlx::query(&raw(200, &format!("'{truth}'")))
        .execute(&t.pool)
        .await
        .unwrap();
    let (facts, condition): (Value, String) = sqlx::query_as(
        "SELECT negative_facts, negative_condition FROM stock_entry WHERE line_no = 200",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(facts["short_by"], "16");
    assert_eq!(condition, "facts.short_by <= 1");
    t.stop().await;
}

#[tokio::test]
async fn lines_of_one_posting_go_below_zero_one_after_the_other() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    allow(&t, &f, json!("facts.short_by <= 5")).await;
    f.adjust(&t, "4", Some("3.00")).await;
    // Two lines of the same widgets: the first leaves 1, the second takes
    // the level to -2, asked with what the first left.
    let body = json!({
        "offset_account_id": f.offset,
        "lines": [
            { "item_id": f.widget, "warehouse_id": f.main, "qty": "-3" },
            { "item_id": f.widget, "warehouse_id": f.main, "qty": "-3" },
        ],
    });
    let (status, posted) = f
        .posted(&t, "stock_adjustment", "stock_adjustment.post", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    assert_eq!(f.level(&t).await, (-2.0, -6.0));
    let entry = f.last_entry(&t).await;
    assert_eq!(entry["negative_facts"]["on_hand"], "1");
    assert_eq!(entry["negative_facts"]["short_by"], "2");
    // Too far for the rule on the second line: nothing of the posting stays.
    let body = json!({
        "offset_account_id": f.offset,
        "lines": [
            { "item_id": f.widget, "warehouse_id": f.main, "qty": "-2" },
            { "item_id": f.widget, "warehouse_id": f.main, "qty": "-2" },
        ],
    });
    let (status, refused) = f
        .posted(&t, "stock_adjustment", "stock_adjustment.post", body)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(f.level(&t).await, (-2.0, -6.0));
    t.stop().await;
}
