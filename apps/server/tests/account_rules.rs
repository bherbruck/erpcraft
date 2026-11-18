//! Account rules: one place that says which account stock is carried on,
//! asked by every module that moves stock.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    widget: String,
    gadget: String,
    main: String,
    cold: String,
    account: BTreeMap<&'static str, String>,
}

async fn setup(t: &TestApp) -> Fixture {
    let c = t.company("ACME", "USD").await;
    let widget = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let gadget = t
        .create("item", json!({ "code": "GADGET", "name": "Gadget" }))
        .await;
    let mut warehouses = vec![];
    for code in ["MAIN", "COLD"] {
        warehouses.push(
            t.create(
                "warehouse",
                json!({ "company_id": c.id, "code": code, "name": code }),
            )
            .await,
        );
    }
    let mut account = BTreeMap::new();
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("1310", "asset"),
        ("1320", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("5000", "expense"),
        ("5100", "expense"),
        ("5200", "expense"),
    ] {
        let created = t
            .create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await;
        account.insert(code, created);
    }
    Fixture {
        c,
        widget,
        gadget,
        cold: warehouses.pop().unwrap(),
        main: warehouses.pop().unwrap(),
        account,
    }
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

impl Fixture {
    async fn rule(&self, t: &TestApp, scope: Option<(&str, &str)>, account: &str) -> String {
        let (scope_type, scope_id) = scope.unzip();
        t.create(
            "account_rule",
            json!({
                "company_id": self.c.id, "purpose": "inventory",
                "scope_type": scope_type, "scope_id": scope_id,
                "account_id": self.account[account],
            }),
        )
        .await
    }

    /// Create a document, then run `command` on it.
    async fn done(&self, t: &TestApp, doc_type: &str, command: &str, body: Value) -> Value {
        let (status, doc) = self.attempt(t, doc_type, command, body).await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }

    async fn attempt(
        &self,
        t: &TestApp,
        doc_type: &str,
        command: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        t.command(&t.admin, command, json!({ "id": id(&draft) }))
            .await
    }

    /// Adjust stock: `unit_cost` for stock added, none for stock removed.
    async fn adjust(
        &self,
        t: &TestApp,
        item: &str,
        warehouse: &str,
        qty: &str,
        unit_cost: Option<&str>,
    ) -> (StatusCode, Value) {
        self.attempt(
            t,
            "stock_adjustment",
            "stock_adjustment.post",
            json!({
                "offset_account_id": self.account["5100"],
                "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": qty, "unit_cost": unit_cost }],
            }),
        )
        .await
    }

    async fn transfer(&self, t: &TestApp, item: &str, from: &str, to: &str, qty: &str) -> Value {
        self.done(
            t,
            "stock_transfer",
            "stock_transfer.post",
            json!({
                "from_warehouse_id": from, "to_warehouse_id": to,
                "lines": [{ "item_id": item, "qty": qty }],
            }),
        )
        .await
    }

    /// The balances of these accounts; the trial balance must balance.
    async fn balances<const N: usize>(&self, t: &TestApp, codes: [&str; N]) -> [f64; N] {
        let (status, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{tb}");
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        codes.map(|code| {
            tb["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["code"] == code)
                .map_or(0.0, |r| dec(&r["balance"]))
        })
    }

    /// What the stock ledger says all stock on hand is worth.
    async fn stock_value(&self, t: &TestApp) -> f64 {
        let (status, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{on_hand}");
        dec(&on_hand["total_value"])
    }
}

#[tokio::test]
async fn the_most_specific_rule_says_where_stock_is_carried() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let stock = ["1300", "1310", "1320"];

    // Without a rule nothing can be posted, and the error says what to do.
    let (status, body) = f.adjust(&t, &f.widget, &f.main, "10", Some("2.00")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "this company has no 'inventory' account set up: \
         add an account rule with the purpose 'inventory'"
    );
    for table in ["posting", "stock_entry", "gl_entry"] {
        assert_eq!(t.count(&format!("SELECT count(*) FROM {table}")).await, 0);
    }
    assert_eq!(
        t.count("SELECT count(*) FROM document WHERE status <> 'draft'")
            .await,
        0
    );

    // The company's default is used where nothing more specific applies.
    f.rule(&t, None, "1300").await;
    let (status, body) = f.adjust(&t, &f.widget, &f.main, "10", Some("2.00")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balances(&t, stock).await, [20.0, 0.0, 0.0]);

    // A rule for a warehouse overrides the default there, and only there.
    f.rule(&t, Some(("warehouse", &f.cold)), "1310").await;
    let (status, body) = f.adjust(&t, &f.widget, &f.cold, "5", Some("2.00")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balances(&t, stock).await, [20.0, 10.0, 0.0]);

    // A rule for an item overrides the rule of the warehouse it is in.
    f.rule(&t, Some(("item", &f.gadget)), "1320").await;
    let (status, body) = f.adjust(&t, &f.gadget, &f.cold, "1", Some("7.00")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balances(&t, stock).await, [20.0, 10.0, 7.0]);
    // Stock removed leaves the account it was carried on.
    let (status, body) = f.adjust(&t, &f.widget, &f.cold, "-1", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balances(&t, stock).await, [20.0, 8.0, 7.0]);

    // A transfer between warehouses on different accounts moves the value
    // from one account to the other.
    let moved = f.transfer(&t, &f.widget, &f.main, &f.cold, "4").await;
    assert_eq!(f.balances(&t, stock).await, [12.0, 16.0, 7.0]);
    assert_eq!(f.stock_value(&t).await, 35.0);
    let entries = t.count("SELECT count(*) FROM gl_entry").await;
    // An item on one account wherever it is moves in the stock ledger alone.
    f.transfer(&t, &f.gadget, &f.cold, &f.main, "1").await;
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, entries);
    assert_eq!(f.balances(&t, stock).await, [12.0, 16.0, 7.0]);
    // Reversing the transfer moves the value back.
    let (status, body) = t
        .command(
            &t.admin,
            "stock_transfer.reverse",
            json!({ "id": id(&moved) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balances(&t, stock).await, [20.0, 8.0, 7.0]);
    assert_eq!(f.stock_value(&t).await, 35.0);

    t.stop().await;
}

#[tokio::test]
async fn a_rule_must_name_an_account_the_company_can_post_to() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let other = t.company("OTHER", "USD").await;
    let rule = |body: Value| {
        let t = &t;
        async move { t.post(&t.admin, "/api/entities/account_rule", body).await }
    };

    // Another company's account, and a group account, are refused.
    let (status, body) =
        rule(json!({ "company_id": f.c.id, "purpose": "inventory", "account_id": other.cash }))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "the account of an account rule must belong to the rule's company"
    );
    let (status, body) =
        rule(json!({ "company_id": f.c.id, "purpose": "inventory", "account_id": f.c.group }))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "account 1 is a group account and cannot be posted to"
    );

    // One answer per question; a scope is whole or absent; a purpose is a
    // lowercase name.
    let default = f.rule(&t, None, "1300").await;
    let (status, body) = rule(
        json!({ "company_id": f.c.id, "purpose": "inventory", "account_id": f.account["1310"] }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "an account rule with this Company, Purpose, Scope type and Scope already exists"
    );
    for bad in [
        json!({ "purpose": "inventory", "scope_type": "item" }),
        json!({ "purpose": "inventory", "scope_id": f.widget }),
        json!({ "purpose": "Inventory" }),
    ] {
        let mut body = bad;
        body["company_id"] = json!(f.c.id);
        body["account_id"] = json!(f.account["1310"]);
        let (status, body) = rule(body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(t.count("SELECT count(*) FROM account_rule").await, 1);

    // The same checks hold when a rule is changed.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account_rule/{default}"),
            json!({ "account_id": other.cash }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Rules belong to a company: whoever may only read one company's does
    // not see another's.
    let reader = t
        .user_with_role("reader@example.com", &["account_rule:read"], &other.id)
        .await;
    let (status, page) = t.get(&reader, "/api/entities/account_rule").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 0);

    t.stop().await;
}

#[tokio::test]
async fn the_inventory_account_is_worth_what_the_stock_is() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // One default rule, and no module has an inventory account of its own.
    f.rule(&t, None, "1300").await;
    t.create(
        "purchasing_settings",
        json!({
            "company_id": f.c.id,
            "grni_account_id": f.account["2100"],
            "payable_account_id": f.account["2000"],
            "price_variance_account_id": f.account["5200"],
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": f.c.id,
            "receivable_account_id": f.account["1200"],
            "revenue_account_id": f.c.revenue,
            "cogs_account_id": f.account["5000"],
        }),
    )
    .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyer LLC" }))
        .await;
    let agree = |expected: f64| {
        let (t, f) = (&t, &f);
        async move {
            let [inventory] = f.balances(t, ["1300"]).await;
            assert_eq!(inventory, expected);
            assert_eq!(f.stock_value(t).await, expected);
        }
    };

    // A purchase receipt: 10 widgets at 3.00.
    let order = f
        .done(
            &t,
            "purchase_order",
            "purchase_order.approve",
            json!({ "vendor_id": vendor, "lines": [
                { "item_id": f.widget, "warehouse_id": f.main, "qty": "10", "unit_price": "3.00" },
            ]}),
        )
        .await;
    f.done(
        &t,
        "purchase_receipt",
        "purchase_receipt.post",
        json!({ "purchase_order_id": order["id"], "lines": [
            { "order_line_id": order["body"]["lines"][0]["id"], "qty": "10" },
        ]}),
    )
    .await;
    agree(30.0).await;

    // An adjustment: 5 gadgets found, at 2.00.
    let (status, body) = f.adjust(&t, &f.gadget, &f.main, "5", Some("2.00")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    agree(40.0).await;

    // A sale: 4 widgets delivered, at their cost of 3.00.
    let order = f
        .done(
            &t,
            "sales_order",
            "sales_order.approve",
            json!({ "customer_id": customer, "lines": [
                { "item_id": f.widget, "warehouse_id": f.main, "qty": "4", "unit_price": "9.00" },
            ]}),
        )
        .await;
    let delivery = f
        .done(
            &t,
            "sales_delivery",
            "sales_delivery.post",
            json!({ "sales_order_id": order["id"], "lines": [
                { "order_line_id": order["body"]["lines"][0]["id"], "qty": "4" },
            ]}),
        )
        .await;
    agree(28.0).await;

    // A production order: 2 kits, each made of 2 gadgets. The value stays
    // in stock, and on the one account.
    let kit = t
        .create("item", json!({ "code": "KIT", "name": "Kit" }))
        .await;
    let bom = t
        .create(
            "bom",
            json!({ "item_id": kit, "code": "KIT-1", "name": "Kit" }),
        )
        .await;
    t.create(
        "bom_line",
        json!({ "bom_id": bom, "item_id": f.gadget, "qty": "2" }),
    )
    .await;
    let made = f
        .done(
            &t,
            "production_order",
            "production_order.release",
            json!({ "bom_id": bom, "qty": "2", "warehouse_id": f.main }),
        )
        .await;
    let entries = t.count("SELECT count(*) FROM gl_entry").await;
    let (status, body) = t
        .command(
            &t.admin,
            "production_order.complete",
            json!({ "id": id(&made) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, entries);
    agree(28.0).await;

    // Stock written off, and a delivery taken back.
    let (status, body) = f.adjust(&t, &kit, &f.main, "-1", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    agree(24.0).await;
    let (status, body) = t
        .command(
            &t.admin,
            "sales_delivery.reverse",
            json!({ "id": id(&delivery) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    agree(36.0).await;

    // Giving the kits an account of their own from now on does not move
    // what is already there, but the two accounts together still carry
    // exactly the stock.
    f.rule(&t, Some(("item", &kit)), "1310").await;
    let (status, body) = f.adjust(&t, &f.gadget, &f.main, "3", Some("2.00")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    agree(42.0).await;
    let again = f
        .done(
            &t,
            "production_order",
            "production_order.release",
            json!({ "bom_id": bom, "qty": "1", "warehouse_id": f.main }),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "production_order.complete",
            json!({ "id": id(&again) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let [inventory, kits] = f.balances(&t, ["1300", "1310"]).await;
    assert_eq!((inventory, kits), (38.0, 4.0));
    assert_eq!(f.stock_value(&t).await, 42.0);

    t.stop().await;
}
