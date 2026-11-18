//! Planning: draft orders count as planned, so running the draft commands
//! again or at once does not order twice; whether an item is bought or made
//! is the reorder rule's choice; a price nobody agreed to is never invented;
//! what a caller may not read is not shown.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec};
use serde_json::{Value, json};

struct F {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
}

async fn setup(t: &TestApp) -> F {
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Parts Inc" }))
        .await;
    F {
        c,
        item,
        warehouse,
        vendor,
    }
}

impl F {
    async fn rule(&self, t: &TestApp, extra: Value) -> String {
        let mut rule = json!({ "company_id": self.c.id, "item_id": self.item,
                               "warehouse_id": self.warehouse, "min_qty": "10" });
        for (k, v) in extra.as_object().unwrap() {
            rule[k] = v.clone();
        }
        t.create("reorder_rule", rule).await
    }

    async fn bill(&self, t: &TestApp) -> String {
        t.create(
            "bom",
            json!({ "item_id": self.item, "code": "W-1", "name": "Widget" }),
        )
        .await
    }

    async fn run(&self, t: &TestApp, token: &str, command: &str) -> (StatusCode, Value) {
        t.command(
            token,
            command,
            json!({ "company_id": self.c.id, "doc_date": "2026-01-06" }),
        )
        .await
    }

    async fn ok(&self, t: &TestApp, command: &str) -> Value {
        let (status, out) = self.run(t, &t.admin, command).await;
        assert_eq!(status, StatusCode::OK, "{command}: {out}");
        out
    }

    async fn suggestions(&self, t: &TestApp, token: &str) -> Vec<Value> {
        let (status, out) = t
            .command(
                token,
                "planning.suggestions",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        out["suggestions"].as_array().unwrap().clone()
    }
}

async fn count(t: &TestApp, doc_type: &str) -> i64 {
    t.count(&format!(
        "SELECT count(*) FROM document WHERE doc_type = '{doc_type}'"
    ))
    .await
}

/// A drafted purchase order is planned: availability shows it apart from
/// what is on order, and the next run drafts nothing more.
#[tokio::test]
async fn drafting_purchase_orders_twice_does_not_order_twice() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.rule(&t, json!({ "vendor_id": f.vendor, "unit_price": "2.00" }))
        .await;
    let first = f.ok(&t, "planning.draft_purchase_orders").await;
    assert_eq!(first["purchase_order_ids"].as_array().unwrap().len(), 1);
    let second = f.ok(&t, "planning.draft_purchase_orders").await;
    assert!(
        second["purchase_order_ids"].as_array().unwrap().is_empty(),
        "{second}"
    );
    assert_eq!(count(&t, "purchase_order").await, 1);
    assert!(f.suggestions(&t, &t.admin).await.is_empty());

    let (status, out) = t
        .command(
            &t.admin,
            "stock.availability",
            json!({ "company_id": f.c.id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let row = &out["rows"][0];
    assert_eq!(dec(&row["on_order"]), 0.0, "a draft is not on order: {row}");
    assert_eq!(dec(&row["planned"]), 10.0, "{row}");
    assert_eq!(dec(&row["projected"]), 0.0, "{row}");
    t.stop().await;
}

/// Two runs at once draft one order between them: the second waits for the
/// first and counts what it drafted.
#[tokio::test]
async fn two_runs_at_once_draft_once() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.rule(&t, json!({ "vendor_id": f.vendor, "unit_price": "2.00" }))
        .await;
    let (a, b) = tokio::join!(
        f.run(&t, &t.admin, "planning.draft_purchase_orders"),
        f.run(&t, &t.admin, "planning.draft_purchase_orders"),
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);
    assert_eq!(count(&t, "purchase_order").await, 1);

    // And the same for what is made.
    let gadget = t
        .create("item", json!({ "code": "GADGET", "name": "Gadget" }))
        .await;
    t.create(
        "bom",
        json!({ "item_id": gadget, "code": "G-1", "name": "Gadget" }),
    )
    .await;
    t.create(
        "reorder_rule",
        json!({ "company_id": f.c.id, "item_id": gadget, "warehouse_id": f.warehouse,
                "min_qty": "5" }),
    )
    .await;
    let (a, b) = tokio::join!(
        f.run(&t, &t.admin, "planning.draft_production_orders"),
        f.run(&t, &t.admin, "planning.draft_production_orders"),
    );
    assert_eq!(a.0, StatusCode::OK, "{}", a.1);
    assert_eq!(b.0, StatusCode::OK, "{}", b.1);
    assert_eq!(count(&t, "production_order").await, 1);
    f.ok(&t, "planning.draft_production_orders").await;
    assert_eq!(count(&t, "production_order").await, 1);
    t.stop().await;
}

/// Without a price of its own or one paid before, a suggestion has none,
/// and no line is drafted at 0.00. A draft's price is not a price paid;
/// an order placed is.
#[tokio::test]
async fn a_missing_price_stays_missing() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.rule(&t, json!({ "vendor_id": f.vendor })).await;
    assert_eq!(
        f.suggestions(&t, &t.admin).await[0]["unit_price"],
        Value::Null
    );
    let out = f.ok(&t, "planning.draft_purchase_orders").await;
    assert!(
        out["purchase_order_ids"].as_array().unwrap().is_empty(),
        "{out}"
    );
    assert_eq!(out["without_price"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(count(&t, "purchase_order").await, 0);

    // A draft at 3.00 elsewhere is not a price paid.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({ "company_id": f.c.id, "doc_date": "2026-01-02", "body": {
                "vendor_id": f.vendor,
                "lines": [{ "item_id": f.item, "warehouse_id": f.warehouse, "qty": "1",
                            "unit_price": "3.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let list = f.suggestions(&t, &t.admin).await;
    assert_eq!(list[0]["unit_price"], Value::Null, "{list:?}");
    // Placed, it is.
    let account = |code: &'static str, kind: &'static str| {
        let company = f.c.id.clone();
        let t = &t;
        async move {
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": code, "account_type": kind }),
            )
            .await
        }
    };
    let grni = account("2100", "liability").await;
    let payable = account("2000", "liability").await;
    let variance = account("5200", "expense").await;
    t.create(
        "purchasing_settings",
        json!({ "company_id": f.c.id, "grni_account_id": grni, "payable_account_id": payable,
                "price_variance_account_id": variance }),
    )
    .await;
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let list = f.suggestions(&t, &t.admin).await;
    assert_eq!(dec(&list[0]["unit_price"]), 3.0, "{list:?}");
    t.stop().await;
}

/// The rule's vendor and price are shown only to someone who may read
/// reorder rules, and drafting from the rules takes that right. A
/// suggestion that comes from a rule alone still names its warehouse.
#[tokio::test]
async fn suggestions_respect_reorder_rule_rights() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.rule(&t, json!({ "vendor_id": f.vendor, "unit_price": "7.25" }))
        .await;
    let base = [
        "stock_entry:read",
        "sales_order:read",
        "purchase_order:read",
        "production_order:read",
        "purchase_order:create",
        "warehouse:read",
    ];
    let planner = t
        .user_with_role("planner@example.com", &base, &f.c.id)
        .await;
    let (status, _) = t.get(&planner, "/api/entities/reorder_rule").await;
    assert_ne!(status, StatusCode::OK);
    let list = f.suggestions(&t, &planner).await;
    let s = &list[0];
    assert!(
        s["unit_price"].is_null() && s["vendor_id"].is_null(),
        "rule's vendor and price shown without reorder_rule:read: {s}"
    );
    assert_eq!(s["warehouse_code"], "MAIN", "{s}");
    let (status, out) = f.run(&t, &planner, "planning.draft_purchase_orders").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");

    let mut more = base.to_vec();
    more.push("reorder_rule:read");
    let reader = t.user_with_role("reader@example.com", &more, &f.c.id).await;
    let list = f.suggestions(&t, &reader).await;
    assert_eq!(list[0]["vendor_id"], f.vendor.as_str());
    assert_eq!(dec(&list[0]["unit_price"]), 7.25);
    assert_eq!(list[0]["warehouse_code"], "MAIN");
    let (status, out) = f.run(&t, &reader, "planning.draft_purchase_orders").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["purchase_order_ids"].as_array().unwrap().len(), 1);
    t.stop().await;
}

/// Bought or made is the rule's choice. Left open: bought from the rule's
/// vendor, made by an active bill when there is no vendor, bought
/// otherwise.
#[tokio::test]
async fn the_rule_says_whether_an_item_is_bought_or_made() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let source = |list: Vec<Value>| list[0]["source"].as_str().unwrap().to_owned();

    // No bill, no vendor: bought.
    let rule = f.rule(&t, json!({})).await;
    assert_eq!(source(f.suggestions(&t, &t.admin).await), "buy");
    // Made, explicitly, without a bill: said, and not drafted.
    let set = |procure: Value, vendor: Value| {
        let rule = rule.clone();
        let t = &t;
        async move {
            let (status, body) = t
                .patch(
                    &t.admin,
                    &format!("/api/entities/reorder_rule/{rule}"),
                    json!({ "procure": procure, "vendor_id": vendor }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    };
    set(json!("make"), Value::Null).await;
    let list = f.suggestions(&t, &t.admin).await;
    assert_eq!(list[0]["source"], "make");
    assert!(list[0]["note"].as_str().unwrap().contains("no active bill"));
    let out = f.ok(&t, "planning.draft_production_orders").await;
    assert!(out["production_order_ids"].as_array().unwrap().is_empty());
    assert_eq!(out["without_bill"].as_array().unwrap().len(), 1, "{out}");

    // With a bill and no vendor, left open: made.
    f.bill(&t).await;
    set(Value::Null, Value::Null).await;
    assert_eq!(source(f.suggestions(&t, &t.admin).await), "make");
    // A vendor on the rule: bought, though the item has a bill.
    set(Value::Null, json!(f.vendor)).await;
    let list = f.suggestions(&t, &t.admin).await;
    assert_eq!(list[0]["source"], "buy");
    assert_eq!(list[0]["vendor_id"], f.vendor.as_str());
    // Explicitly made, though it names a vendor.
    set(json!("make"), json!(f.vendor)).await;
    assert_eq!(source(f.suggestions(&t, &t.admin).await), "make");
    // Explicitly bought, though it has a bill.
    set(json!("buy"), json!(f.vendor)).await;
    let list = f.suggestions(&t, &t.admin).await;
    assert_eq!(list[0]["source"], "buy");
    let out = f.ok(&t, "planning.draft_production_orders").await;
    assert!(out["production_order_ids"].as_array().unwrap().is_empty());
    t.stop().await;
}

/// A rule naming a vendor and a price is bought even when the item has an
/// active bill of materials (made in another company, or once in house).
#[tokio::test]
async fn a_rule_with_a_vendor_is_bought_not_made() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.bill(&t).await;
    f.rule(&t, json!({ "vendor_id": f.vendor, "unit_price": "2.00" }))
        .await;
    let bought = f.ok(&t, "planning.draft_purchase_orders").await;
    let made = f.ok(&t, "planning.draft_production_orders").await;
    assert_eq!(
        (
            bought["purchase_order_ids"].as_array().unwrap().len(),
            made["production_order_ids"].as_array().unwrap().len()
        ),
        (1, 0),
        "{bought} {made}"
    );
    t.stop().await;
}

/// Drafting production orders twice makes once; an inactive bill is not
/// used; it takes the right to read bills, which are shared by all
/// companies, and is scoped to the company.
#[tokio::test]
async fn drafting_production_orders_is_planned_and_scoped() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.rule(&t, json!({})).await;
    t.create(
        "bom",
        json!({ "item_id": f.item, "code": "W-0", "name": "Old", "active": false }),
    )
    .await;
    let made = f.ok(&t, "planning.draft_production_orders").await;
    assert!(
        made["production_order_ids"].as_array().unwrap().is_empty(),
        "{made}"
    );

    f.bill(&t).await;
    let rights = json!([
        "production_order:create",
        "production_order:read",
        "stock_entry:read",
        "sales_order:read",
        "purchase_order:read",
        "reorder_rule:read",
        "item:read",
        "warehouse:read"
    ]);
    let (no_bills, _) = t
        .user_with_rules("nobills@example.com", rights.clone(), None)
        .await;
    let (status, out) = f
        .run(&t, &no_bills, "planning.draft_production_orders")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    let other = t.company("OTHER", "USD").await;
    let mut with_bills = rights.as_array().unwrap().clone();
    with_bills.push(json!("bom:read"));
    let (elsewhere, _) = t
        .user_with_rules("elsewhere@example.com", json!(with_bills), Some(&other.id))
        .await;
    let (status, out) = f
        .run(&t, &elsewhere, "planning.draft_production_orders")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{out}");
    let (planner, _) = t
        .user_with_rules("planner@example.com", json!(with_bills), None)
        .await;
    let (status, out) = f
        .run(&t, &planner, "planning.draft_production_orders")
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["production_order_ids"].as_array().unwrap().len(), 1);
    f.ok(&t, "planning.draft_production_orders").await;
    assert_eq!(count(&t, "production_order").await, 1);
    t.stop().await;
}
