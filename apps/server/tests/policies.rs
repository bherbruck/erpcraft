//! Policies: the business judgements a company configures with rules:
//! receiving or delivering more than was ordered, an invoice price that
//! differs from the order price, an inactive vendor or customer, a closed
//! order, and the date of a posting.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

const OVER_RECEIPT: &str = "purchase_receipt.over_receipt";
const OVER_DELIVERY: &str = "sales_delivery.over_delivery";
const PRICE_DIFFERS: &str = "vendor_invoice.price_differs";
const SALES_PRICE_DIFFERS: &str = "sales_invoice.price_differs";
const VENDOR_INACTIVE: &str = "vendor.inactive";
const CUSTOMER_INACTIVE: &str = "customer.inactive";
const PURCHASE_ORDER_CLOSED: &str = "purchase_order.closed";
const SALES_ORDER_CLOSED: &str = "sales_order.closed";
const POSTING_DATE: &str = "posting.date";

/// What every company shares.
struct Shared {
    item: String,
    vendor: String,
    customer: String,
}

/// One company set up to buy and to sell.
struct Fixture {
    c: Company,
    item: String,
    vendor: String,
    customer: String,
    warehouse: String,
}

async fn shared(t: &TestApp) -> Shared {
    Shared {
        item: t
            .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
            .await,
        vendor: t
            .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
            .await,
        customer: t
            .create("customer", json!({ "code": "C001", "name": "Buyers Ltd" }))
            .await,
    }
}

async fn company(t: &TestApp, shared: &Shared, code: &str) -> Fixture {
    let c = t.company(code, "USD").await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
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
    let variance = account("5200", "Purchase price variance", "expense").await;
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "grni_account_id": grni,
            "payable_account_id": payable,
            "price_variance_account_id": variance,
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": receivable,
            "revenue_account_id": c.revenue,
            "cogs_account_id": cogs,
        }),
    )
    .await;
    t.inventory_rule(&c.id, &inventory).await;
    Fixture {
        c,
        item: shared.item.clone(),
        vendor: shared.vendor.clone(),
        customer: shared.customer.clone(),
        warehouse,
    }
}

async fn setup(t: &TestApp) -> Fixture {
    let shared = shared(t).await;
    company(t, &shared, "ACME").await
}

/// Save a policy rule as the administrator; returns the response.
async fn rule(t: &TestApp, data: Value) -> (StatusCode, Value) {
    t.post(&t.admin, "/api/entities/policy_rule", data).await
}

/// Save a policy rule that must be accepted; returns its id.
async fn saved(t: &TestApp, data: Value) -> String {
    let (status, body) = rule(t, data).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    id(&body)
}

/// The message of a rule that must be refused when saved.
async fn unsaved(t: &TestApp, data: Value) -> String {
    let (status, body) = rule(t, data).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    body["error"]["message"].as_str().unwrap().to_owned()
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

impl Fixture {
    /// Draft a document and post it with `command`. A draft that cannot
    /// be created is the answer.
    async fn posted(
        &self,
        t: &TestApp,
        token: &str,
        doc_type: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, doc) = t
            .post(
                token,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        if status != StatusCode::CREATED {
            return (status, doc);
        }
        t.command(
            token,
            &format!("{doc_type}.post"),
            json!({ "id": id(&doc) }),
        )
        .await
    }

    /// An approved order of `doc_type` for `qty` widgets at `price`.
    async fn order(&self, t: &TestApp, doc_type: &str, qty: &str, price: &str) -> Value {
        let party = match doc_type {
            "purchase_order" => json!({ "vendor_id": self.vendor }),
            _ => json!({ "customer_id": self.customer }),
        };
        let mut body = json!({
            "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
        });
        body.as_object_mut()
            .unwrap()
            .extend(party.as_object().unwrap().clone());
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, order) = t
            .command(
                &t.admin,
                &format!("{doc_type}.approve"),
                json!({ "id": id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{order}");
        order
    }

    async fn purchase_order(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        self.order(t, "purchase_order", qty, price).await
    }

    async fn receive_as(
        &self,
        t: &TestApp,
        token: &str,
        order: &Value,
        qty: &str,
    ) -> (StatusCode, Value) {
        let body = json!({
            "purchase_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
        });
        self.posted(t, token, "purchase_receipt", body).await
    }

    async fn receive(&self, t: &TestApp, order: &Value, qty: &str) -> (StatusCode, Value) {
        self.receive_as(t, &t.admin, order, qty).await
    }

    async fn vendor_invoice(
        &self,
        t: &TestApp,
        order: &Value,
        qty: &str,
        price: &str,
    ) -> (StatusCode, Value) {
        let body = json!({
            "purchase_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty, "unit_price": price }],
        });
        self.posted(t, &t.admin, "vendor_invoice", body).await
    }

    async fn deliver(&self, t: &TestApp, order: &Value, qty: &str) -> (StatusCode, Value) {
        let body = json!({
            "sales_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
        });
        self.posted(t, &t.admin, "sales_delivery", body).await
    }

    async fn sales_invoice(
        &self,
        t: &TestApp,
        order: &Value,
        qty: &str,
        price: &str,
    ) -> (StatusCode, Value) {
        let body = json!({
            "sales_order_id": order["id"],
            "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty, "unit_price": price }],
        });
        self.posted(t, &t.admin, "sales_invoice", body).await
    }

    /// The balance of an account, after checking that the books balance
    /// and that the inventory account equals the value of the stock.
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

    async fn on_hand(&self, t: &TestApp) -> f64 {
        let (_, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        on_hand["rows"]
            .as_array()
            .unwrap()
            .first()
            .map_or(0.0, |r| dec(&r["qty"]))
    }

    async fn now(&self, t: &TestApp, order: &Value) -> Value {
        let doc_type = if order["body"]["vendor_id"].is_string() {
            "purchase_order"
        } else {
            "sales_order"
        };
        let (status, doc) = t
            .get(
                &t.admin,
                &format!("/api/documents/{doc_type}/{}", id(order)),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }
}

#[tokio::test]
async fn with_no_rules_the_defaults_decide_as_before() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    assert_eq!(t.count("SELECT count(*) FROM policy_rule").await, 0);

    // Receiving: within the order yes, beyond it no, in the words used
    // before there were policies.
    let order = f.purchase_order(&t, "10", "2.50").await;
    let (status, body) = f.receive(&t, &order, "4").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.receive(&t, &order, "7").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 6 are still to be received on order line 1, not 7"
    );
    let (status, body) = f.receive(&t, &order, "6").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.receive(&t, &order, "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 0 are still to be received on order line 1, not 1"
    );

    // An invoice at twice the order price is accepted: nothing checks it.
    let (status, body) = f.vendor_invoice(&t, &order, "10", "5.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.now(&t, &order).await["status"], "closed");
    assert_eq!(f.balance(&t, "5200").await, 25.0);

    // Delivering: the same.
    let order = f.order(&t, "sales_order", "5", "9.00").await;
    let (status, body) = f.deliver(&t, &order, "6").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 5 are still to be delivered on order line 1, not 6"
    );
    let (status, body) = f.deliver(&t, &order, "5").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_company_accepts_five_percent_more_than_it_ordered() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_RECEIPT, "effect": "allow",
            "condition": "facts.over_percent <= 5", "priority": 10,
        }),
    )
    .await;
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_RECEIPT, "effect": "refuse", "condition": null,
            "message": "we accept at most 5% more than we ordered; this would be {over_by} more ({over_percent}%)",
            "priority": 20,
        }),
    )
    .await;

    // Six percent over is refused, in the company's own words. The
    // administrator is asking: a policy holds for everyone.
    let order = f.purchase_order(&t, "100", "2.00").await;
    let (status, body) = f.receive(&t, &order, "106").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: we accept at most 5% more than we ordered; this would be 6 more (6%)"
    );
    assert_eq!(f.on_hand(&t).await, 0.0);

    // Five percent over is accepted, also across two receipts. All of it
    // comes into stock at the order price and is owed but not yet billed.
    let (status, body) = f.receive(&t, &order, "60").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.receive(&t, &order, "46").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = f.receive(&t, &order, "45").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.on_hand(&t).await, 105.0);
    assert_eq!(f.balance(&t, "1300").await, 210.0);
    assert_eq!(f.balance(&t, "2100").await, -210.0);
    let now = f.now(&t, &order).await;
    assert_eq!(dec(&now["body"]["lines"][0]["qty"]), 100.0);
    assert_eq!(dec(&now["body"]["lines"][0]["qty_received"]), 105.0);
    assert_eq!(now["status"], "open");

    // The line is over-received already: nothing more comes in.
    let (status, body) = f.receive(&t, &order, "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // What was received can be invoiced, the extra goods too, but no more.
    let (status, body) = f.vendor_invoice(&t, &order, "106", "2.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Invoicing the quantity ordered does not complete the order: five
    // have been received and are not yet invoiced.
    let (status, body) = f.vendor_invoice(&t, &order, "100", "2.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.now(&t, &order).await["status"], "open");
    assert_eq!(f.balance(&t, "2100").await, -10.0);
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_order.close",
            json!({ "id": id(&order) }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // With the rest invoiced the order closes by itself, and nothing is
    // left in goods received not invoiced.
    let (status, invoice) = f.vendor_invoice(&t, &order, "5", "2.00").await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    let now = f.now(&t, &order).await;
    assert_eq!(now["status"], "closed");
    assert_eq!(dec(&now["body"]["lines"][0]["qty_invoiced"]), 105.0);
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -210.0);
    assert_eq!(f.balance(&t, "1300").await, 210.0);
    assert_eq!(f.balance(&t, "5200").await, 0.0);

    // Taking the last invoice back reopens the order, as for any order.
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.reverse",
            json!({ "id": id(&invoice) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.now(&t, &order).await["status"], "open");
    assert_eq!(f.balance(&t, "2100").await, -10.0);
    t.stop().await;
}

#[tokio::test]
async fn a_rule_for_all_companies_applies_until_a_company_has_its_own() {
    let t = TestApp::start().await;
    let shared = shared(&t).await;
    let a = company(&t, &shared, "ACME").await;
    let b = company(&t, &shared, "BOLT").await;

    // For every company: up to ten percent more is accepted.
    saved(
        &t,
        json!({
            "point": OVER_RECEIPT, "effect": "allow",
            "condition": "facts.over_percent <= 10", "priority": 50,
        }),
    )
    .await;
    let order_a = a.purchase_order(&t, "100", "1.00").await;
    let order_b = b.purchase_order(&t, "100", "1.00").await;
    let (status, body) = a.receive(&t, &order_a, "50").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = b.receive(&t, &order_b, "50").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = b.receive(&t, &order_b, "61").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 50 are still to be received on order line 1, not 61"
    );

    // BOLT accepts nothing beyond its orders. At the same priority a
    // company's own rule comes before the one for all companies.
    let own = saved(
        &t,
        json!({
            "company_id": b.c.id, "point": OVER_RECEIPT, "effect": "refuse",
            "condition": "facts.over_by > 0", "message": "BOLT takes exactly what it ordered",
            "priority": 50,
        }),
    )
    .await;
    let (status, body) = b.receive(&t, &order_b, "55").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "line 1: BOLT takes exactly what it ordered");
    // ACME is not affected.
    let (status, body) = a.receive(&t, &order_a, "55").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Priority comes first: a rule for all companies that is looked at
    // earlier wins over the company's own.
    saved(
        &t,
        json!({
            "point": OVER_RECEIPT, "effect": "allow",
            "condition": "facts.over_percent <= 2", "priority": 10,
        }),
    )
    .await;
    let (status, body) = b.receive(&t, &order_b, "53").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = b.receive(&t, &order_b, "52").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // And the company overrides that by putting its own rule earlier still.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{own}"),
            json!({ "priority": 5 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_b = b.purchase_order(&t, "100", "1.00").await;
    let (status, body) = b.receive(&t, &order_b, "101").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "line 1: BOLT takes exactly what it ordered");
    t.stop().await;
}

#[tokio::test]
async fn a_rule_that_could_never_work_is_refused_when_saved() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    let why = unsaved(
        &t,
        json!({ "point": "purchase_receipt.overreceipt", "effect": "allow", "condition": null }),
    )
    .await;
    assert!(
        why.starts_with(
            "there is no policy point 'purchase_receipt.overreceipt'; the policy points are: "
        ) && why.contains(OVER_RECEIPT),
        "{why}"
    );

    let why = unsaved(
        &t,
        json!({ "point": OVER_RECEIPT, "effect": "allow", "condition": "facts.over_pct <= 5" }),
    )
    .await;
    assert_eq!(
        why,
        "expression `facts.over_pct <= 5`: the policy point 'purchase_receipt.over_receipt' \
         supplies no fact 'over_pct'; its facts are: ordered, already_received, receiving, \
         over_by, over_percent"
    );

    // A fact of another point is unknown here.
    let why = unsaved(
        &t,
        json!({ "point": OVER_RECEIPT, "effect": "refuse", "condition": "facts.difference > 0" }),
    )
    .await;
    assert!(why.contains("supplies no fact 'difference'"), "{why}");

    for (broken, expected) in [
        (
            "facts.over_percent <=",
            "expression `facts.over_percent <=`",
        ),
        (
            "record.qty > 5",
            "unknown name `record`; use `facts` or `user`",
        ),
        ("facts.over_percent.round() > 5", "`round` cannot be used"),
        ("  ", "cannot be empty"),
    ] {
        let why = unsaved(
            &t,
            json!({ "point": OVER_RECEIPT, "effect": "allow", "condition": broken }),
        )
        .await;
        assert!(why.contains(expected), "{broken}: {why}");
    }

    let why = unsaved(
        &t,
        json!({ "point": OVER_RECEIPT, "effect": "warn", "condition": null }),
    )
    .await;
    assert_eq!(why, "field 'effect' must be one of: allow, refuse, approve");

    // The condition is checked against the point's facts, so the two are
    // sent together, on a change as well.
    let why = unsaved(&t, json!({ "point": OVER_RECEIPT, "effect": "allow" })).await;
    assert!(
        why.starts_with("send 'condition' together with 'point'"),
        "{why}"
    );
    let good = saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_RECEIPT, "effect": "allow",
            "condition": "facts.over_percent <= 5",
        }),
    )
    .await;
    let path = format!("/api/entities/policy_rule/{good}");
    let (status, body) = t
        .patch(
            &t.admin,
            &path,
            json!({ "condition": "facts.difference_percent <= 5" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).starts_with("send 'point' together with 'condition'"),
        "{body}"
    );
    let (status, body) = t
        .patch(
            &t.admin,
            &path,
            json!({ "point": OVER_RECEIPT, "condition": "facts.difference_percent <= 5" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).contains("supplies no fact 'difference_percent'"),
        "{body}"
    );
    let (status, body) = t
        .patch(
            &t.admin,
            &path,
            json!({ "point": OVER_RECEIPT, "condition": "facts.over_percent <= 3" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["condition"], "facts.over_percent <= 3");

    // Only the good rule was stored, and it is in force.
    assert_eq!(t.count("SELECT count(*) FROM policy_rule").await, 1);
    let order = f.purchase_order(&t, "100", "1.00").await;
    let (status, body) = f.receive(&t, &order, "104").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = f.receive(&t, &order, "103").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn an_inactive_rule_is_ignored() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let rule = saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_RECEIPT, "effect": "allow",
            "condition": "facts.over_percent <= 5", "active": false,
        }),
    )
    .await;
    let order = f.purchase_order(&t, "100", "1.00").await;
    let (status, body) = f.receive(&t, &order, "105").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 100 are still to be received on order line 1, not 105"
    );

    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{rule}"),
            json!({ "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.receive(&t, &order, "105").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_rule_that_cannot_be_decided_never_loosens_anything() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (_, me) = t.get(&t.admin, "/api/auth/me").await;
    let admin_id = me["user_id"].as_str().unwrap().to_owned();

    // Allowed for users marked as trusted receivers. The administrator has
    // no such attribute, so the rule cannot be decided for them: it does
    // not allow, and being an administrator does not help.
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_RECEIPT, "effect": "allow",
            "condition": "user.trusted_receiver && facts.over_percent <= 10",
        }),
    )
    .await;
    let order = f.purchase_order(&t, "100", "1.00").await;
    let (status, body) = f.receive(&t, &order, "105").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 100 are still to be received on order line 1, not 105"
    );
    t.set_attributes(&admin_id, json!({ "trusted_receiver": true }))
        .await;
    let (status, body) = f.receive(&t, &order, "105").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A clerk without the attribute is refused where the administrator,
    // who has it, is not: the rule reads the user, not their rights.
    let clerk = t
        .user_with_role("clerk@example.com", &["*:*"], &f.c.id)
        .await;
    let order = f.purchase_order(&t, "100", "1.00").await;
    let (status, body) = f.receive_as(&t, &clerk, &order, "105").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // A refusing rule that cannot be decided refuses, even a receipt well
    // within the order.
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_RECEIPT, "effect": "refuse",
            "condition": "user.receiving_blocked", "message": "you may not receive goods",
            "priority": 1,
        }),
    )
    .await;
    let (status, body) = f.receive(&t, &order, "10").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "line 1: you may not receive goods");
    t.set_attributes(
        &admin_id,
        json!({ "trusted_receiver": true, "receiving_blocked": false }),
    )
    .await;
    let (status, body) = f.receive(&t, &order, "10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn an_invoice_too_far_above_the_order_price_is_refused() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": PRICE_DIFFERS, "effect": "refuse",
            "condition": "facts.difference_percent > 2",
            "message": "the vendor charges {invoice_price}, which is {difference_percent}% above the order price {order_price}; at most 2% is accepted",
        }),
    )
    .await;
    let order = f.purchase_order(&t, "10", "10.00").await;
    let (status, body) = f.receive(&t, &order, "10").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Three percent dearer: refused, and nothing is posted.
    let (status, body) = f.vendor_invoice(&t, &order, "4", "10.30").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: the vendor charges 10.3, which is 3% above the order price 10; at most 2% is accepted"
    );
    assert_eq!(f.balance(&t, "2000").await, 0.0);

    // Two percent dearer is within the limit; cheaper always is.
    let (status, body) = f.vendor_invoice(&t, &order, "4", "10.20").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.vendor_invoice(&t, &order, "6", "8.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balance(&t, "2000").await, -88.8);
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.now(&t, &order).await["status"], "closed");

    // A refusing rule without a message of its own is explained by the
    // module.
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": PRICE_DIFFERS, "effect": "refuse",
            "condition": "facts.difference < 0 - 1", "priority": 1,
        }),
    )
    .await;
    let order = f.purchase_order(&t, "10", "10.00").await;
    let (status, body) = f.receive(&t, &order, "10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.vendor_invoice(&t, &order, "10", "8.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: the invoice price 8 differs too much from the price 10 of order line 1"
    );
    t.stop().await;
}

#[tokio::test]
async fn delivering_more_than_was_ordered_mirrors_receiving() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Something to sell: 200 widgets bought at 1.00.
    let supply = f.purchase_order(&t, "200", "1.00").await;
    let (status, body) = f.receive(&t, &supply, "200").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_DELIVERY, "effect": "allow",
            "condition": "facts.over_percent <= 5", "priority": 10,
        }),
    )
    .await;
    saved(
        &t,
        json!({
            "company_id": f.c.id, "point": OVER_DELIVERY, "effect": "refuse",
            "condition": "facts.over_by > 0",
            "message": "we ship at most 5% more than was ordered, not {over_percent}%",
            "priority": 20,
        }),
    )
    .await;
    // The rules for receiving are another point: receiving is not loosened.
    let (status, body) = f.receive(&t, &supply, "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let order = f.order(&t, "sales_order", "100", "3.00").await;
    let (status, body) = f.deliver(&t, &order, "106").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: we ship at most 5% more than was ordered, not 6%"
    );
    let (status, body) = f.deliver(&t, &order, "105").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.on_hand(&t).await, 95.0);
    assert_eq!(f.balance(&t, "5000").await, 105.0);
    let now = f.now(&t, &order).await;
    assert_eq!(dec(&now["body"]["lines"][0]["qty_delivered"]), 105.0);
    assert_eq!(now["status"], "open");

    // Everything delivered can be invoiced, and no more. The order closes
    // by itself once it is.
    let (status, body) = f.sales_invoice(&t, &order, "106", "3.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = f.sales_invoice(&t, &order, "100", "3.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.now(&t, &order).await["status"], "open");
    let (status, body) = f.sales_invoice(&t, &order, "5", "3.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.now(&t, &order).await["status"], "closed");
    assert_eq!(f.balance(&t, "1200").await, 315.0);
    assert_eq!(f.balance(&t, "4000").await, -315.0);
    t.stop().await;
}

#[tokio::test]
async fn policy_points_are_described_for_interfaces_and_agents() {
    let t = TestApp::start().await;
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    let points = meta["policies"].as_array().unwrap();
    let names: Vec<&str> = points.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "asset.below_minimum",
            "bank_statement.amount_difference",
            CUSTOMER_INACTIVE,
            "document.send",
            "edi.auto_approve",
            "edi.auto_draft",
            "edi.send",
            "edi_invoice.before_asn",
            "edi_order.duplicate",
            "fx.rate_differs",
            "gl.dimensions",
            "payment.discount_tax",
            "payment.late_discount",
            "payment.post",
            "payment.write_off",
            "payment_run.approve",
            POSTING_DATE,
            "production_order.inactive_bom",
            "production_order.inactive_work_center",
            "purchase_order.approve",
            PURCHASE_ORDER_CLOSED,
            "purchase_order.reopen_on_return",
            OVER_RECEIPT,
            "sales_credit.without_invoice",
            "sales_credit_note.without_return",
            OVER_DELIVERY,
            "sales_delivery.reserved_elsewhere",
            SALES_PRICE_DIFFERS,
            SALES_ORDER_CLOSED,
            "sales_order.reopen_on_return",
            "stock.lot_issue",
            "stock.negative",
            "stock_transfer.between_sites",
            "stock_transfer.close_short",
            "tax_code.use",
            VENDOR_INACTIVE,
            "vendor_credit.without_invoice",
            "vendor_credit_note.without_return",
            PRICE_DIFFERS,
            "vendor_invoice.without_order",
        ]
    );
    let point = |name: &str| {
        points
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("{name}"))
            .clone()
    };
    let facts = |point: &Value| -> Vec<(String, String)> {
        point["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                assert!(!f["description"].as_str().unwrap().is_empty(), "{f}");
                (
                    f["name"].as_str().unwrap().to_owned(),
                    f["type"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let named = |list: &[(&str, &str)]| -> Vec<(String, String)> {
        list.iter()
            .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
            .collect()
    };

    let over_receipt = point(OVER_RECEIPT);
    assert_eq!(over_receipt["module"], "purchasing");
    assert!(
        over_receipt["description"]
            .as_str()
            .unwrap()
            .contains("receipt")
    );
    assert_eq!(
        over_receipt["default"],
        json!({ "effect": "refuse", "condition": "facts.over_by > 0" })
    );
    assert_eq!(
        over_receipt["refusal"],
        "more would be received than was ordered"
    );
    assert_eq!(
        facts(&over_receipt),
        named(&[
            ("ordered", "number"),
            ("already_received", "number"),
            ("receiving", "number"),
            ("over_by", "number"),
            ("over_percent", "number"),
        ])
    );
    assert_eq!(
        point(PRICE_DIFFERS)["default"],
        json!({ "effect": "allow", "condition": null })
    );

    // The points that were refusals compiled into the modules, each with
    // the behaviour from before as its default.
    for (name, module, default, expected) in [
        (
            VENDOR_INACTIVE,
            "purchasing",
            json!({ "effect": "refuse", "condition": "facts.doc_type == 'purchase_order' && !facts.active" }),
            named(&[
                ("doc_type", "text"),
                ("vendor_code", "text"),
                ("active", "bool"),
            ]),
        ),
        (
            CUSTOMER_INACTIVE,
            "sales",
            json!({ "effect": "refuse", "condition": "facts.doc_type == 'sales_order' && !facts.active" }),
            named(&[
                ("doc_type", "text"),
                ("customer_code", "text"),
                ("active", "bool"),
            ]),
        ),
        (
            PURCHASE_ORDER_CLOSED,
            "purchasing",
            json!({ "effect": "refuse", "condition": "facts.doc_type == 'purchase_receipt' || facts.to_invoice <= 0" }),
            named(&[
                ("doc_type", "text"),
                ("complete", "bool"),
                ("ordered", "number"),
                ("received", "number"),
                ("invoiced", "number"),
                ("to_invoice", "number"),
            ]),
        ),
        (
            SALES_ORDER_CLOSED,
            "sales",
            json!({ "effect": "refuse", "condition": "facts.doc_type == 'sales_delivery' || facts.to_invoice <= 0" }),
            named(&[
                ("doc_type", "text"),
                ("complete", "bool"),
                ("ordered", "number"),
                ("delivered", "number"),
                ("invoiced", "number"),
                ("to_invoice", "number"),
            ]),
        ),
        (
            SALES_PRICE_DIFFERS,
            "sales",
            json!({ "effect": "allow", "condition": null }),
            named(&[
                ("order_price", "number"),
                ("invoice_price", "number"),
                ("difference", "number"),
                ("difference_percent", "number"),
            ]),
        ),
        (
            POSTING_DATE,
            "kernel",
            json!({ "effect": "allow", "condition": null }),
            named(&[
                ("days_from_today", "number"),
                ("doc_type", "text"),
                ("posting_date", "text"),
                ("reversal", "bool"),
            ]),
        ),
        (
            "production_order.inactive_bom",
            "manufacturing",
            json!({ "effect": "refuse", "condition": "!facts.active" }),
            named(&[("bom_code", "text"), ("active", "bool")]),
        ),
        (
            "tax_code.use",
            "tax",
            json!({ "effect": "refuse", "condition": "!facts.active" }),
            named(&[
                ("code", "text"),
                ("rate", "number"),
                ("active", "bool"),
                ("direction", "text"),
            ]),
        ),
    ] {
        let described = point(name);
        assert_eq!(described["module"], module, "{name}");
        assert_eq!(described["default"], default, "{name}");
        assert_eq!(facts(&described), expected, "{name}");
        assert!(!described["refusal"].as_str().unwrap().is_empty());
    }

    // One module at a time, as for everything else in the description.
    let (_, sales) = t.get(&t.admin, "/api/meta?module=sales").await;
    let sales: Vec<&str> = sales["policies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        sales,
        [
            CUSTOMER_INACTIVE,
            "sales_credit.without_invoice",
            "sales_credit_note.without_return",
            OVER_DELIVERY,
            "sales_delivery.reserved_elsewhere",
            SALES_PRICE_DIFFERS,
            SALES_ORDER_CLOSED,
            "sales_order.reopen_on_return",
        ]
    );

    // The rules themselves are an entity like any other, and over MCP an
    // agent finds the points the same way.
    assert!(
        meta["entities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "policy_rule")
    );
    let described = t
        .tool(&t.admin, "meta_describe", json!({ "module": "purchasing" }))
        .await;
    assert!(described.to_string().contains(PRICE_DIFFERS));
    t.stop().await;
}

impl Fixture {
    /// Draft an order of `doc_type` for `qty` widgets and try to approve it.
    async fn approve(&self, t: &TestApp, doc_type: &str, qty: &str) -> (StatusCode, Value) {
        let party = match doc_type {
            "purchase_order" => json!({ "vendor_id": self.vendor }),
            _ => json!({ "customer_id": self.customer }),
        };
        let mut body = json!({
            "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": "2.50" }],
        });
        body.as_object_mut()
            .unwrap()
            .extend(party.as_object().unwrap().clone());
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        t.command(
            &t.admin,
            &format!("{doc_type}.approve"),
            json!({ "id": id(&draft) }),
        )
        .await
    }

    /// Make a vendor or customer inactive.
    async fn deactivate(&self, t: &TestApp, entity: &str, id: &str) {
        let (status, body) = t
            .patch(
                &t.admin,
                &format!("/api/entities/{entity}/{id}"),
                json!({ "active": false }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// Run `<doc_type>.<action>` on an order.
    async fn step(&self, t: &TestApp, command: &str, order: &Value) -> (StatusCode, Value) {
        t.command(&t.admin, command, json!({ "id": id(order) }))
            .await
    }
}

#[tokio::test]
async fn an_inactive_vendor_or_customer_is_a_judgement_of_the_company() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let purchase = f.purchase_order(&t, "10", "2.50").await;
    let sale = f.order(&t, "sales_order", "10", "9.00").await;
    f.receive(&t, &purchase, "10").await;
    f.deactivate(&t, "vendor", &f.vendor).await;
    f.deactivate(&t, "customer", &f.customer).await;

    // By default, as before: no new order is approved for them, but what
    // was ordered still arrives, goes out and is invoiced.
    let (status, body) = f.approve(&t, "purchase_order", "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "the vendor is inactive");
    let (status, body) = f.approve(&t, "sales_order", "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "the customer is inactive");
    let (status, body) = f.vendor_invoice(&t, &purchase, "4", "2.50").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.deliver(&t, &sale, "2").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A company that takes nothing more from an inactive vendor says so,
    // in its own words.
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": VENDOR_INACTIVE, "effect": "refuse",
                "condition": "!facts.active", "message": "{vendor_code} is no longer a supplier" }),
    )
    .await;
    let (status, body) = f.vendor_invoice(&t, &purchase, "4", "2.50").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "V001 is no longer a supplier");

    // One that still sells to an inactive customer it knows approves the
    // order; another company is not affected.
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": CUSTOMER_INACTIVE, "effect": "allow",
                "condition": "facts.customer_code == 'C001'" }),
    )
    .await;
    let (status, body) = f.approve(&t, "sales_order", "1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "open");
    let other = company(
        &t,
        &Shared {
            item: f.item.clone(),
            vendor: f.vendor.clone(),
            customer: f.customer.clone(),
        },
        "BOLT",
    )
    .await;
    let (status, body) = other.approve(&t, "sales_order", "1").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_closed_order_takes_more_only_where_the_company_allows_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Ten ordered, four received and invoiced, then closed short by hand.
    let order = f.purchase_order(&t, "10", "2.50").await;
    f.receive(&t, &order, "4").await;
    f.vendor_invoice(&t, &order, "4", "2.50").await;
    let (status, body) = f.step(&t, "purchase_order.close", &order).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "closed");

    // By default, as before: nothing more against it.
    let (status, body) = f.receive(&t, &order, "2").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "the purchase order is closed; documents can be created only against an open order"
    );
    let (status, body) = f.step(&t, "purchase_order.receive", &order).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "the purchase order is closed; a document can be drafted only from an open order"
    );
    let (status, body) = f.vendor_invoice(&t, &order, "1", "2.50").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // A company that takes late deliveries on an order it closed short.
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": PURCHASE_ORDER_CLOSED, "effect": "allow",
                "condition": "facts.doc_type == 'purchase_receipt' && !facts.complete" }),
    )
    .await;
    let (status, draft) = f.step(&t, "purchase_order.receive", &order).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["qty"]), 6.0);
    let (status, body) = f.receive(&t, &order, "2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The order stays closed; its line counts the goods, which are owed
    // for until invoiced.
    let now = f.now(&t, &order).await;
    assert_eq!(now["status"], "closed");
    assert_eq!(dec(&now["body"]["lines"][0]["qty_received"]), 6.0);
    assert_eq!(f.balance(&t, "2100").await, -5.0);
    assert_eq!(f.on_hand(&t).await, 6.0);

    // What it holds received and not invoiced can be invoiced by default,
    // and no more than that.
    let (status, body) = f.vendor_invoice(&t, &order, "3", "2.50").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = f.vendor_invoice(&t, &order, "2", "2.50").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -15.0);
    assert_eq!(f.now(&t, &order).await["status"], "closed");
    let (status, body) = f.vendor_invoice(&t, &order, "1", "2.50").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Sales mirrors it; a refusal can carry the company's words.
    let sale = f.order(&t, "sales_order", "5", "9.00").await;
    f.deliver(&t, &sale, "2").await;
    f.sales_invoice(&t, &sale, "2", "9.00").await;
    let (status, body) = f.step(&t, "sales_order.close", &sale).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.deliver(&t, &sale, "1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        message(&body),
        "the sales order is closed; documents can be created only against an open order"
    );
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": SALES_ORDER_CLOSED, "effect": "refuse",
                "condition": "facts.delivered >= 3", "priority": 1,
                "message": "{delivered} of {ordered} went out; the rest is cancelled" }),
    )
    .await;
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": SALES_ORDER_CLOSED, "effect": "allow",
                "condition": "facts.doc_type == 'sales_delivery'" }),
    )
    .await;
    let (status, body) = f.deliver(&t, &sale, "1").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = f.deliver(&t, &sale, "1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(message(&body), "3 of 5 went out; the rest is cancelled");
    assert_eq!(f.now(&t, &sale).await["status"], "closed");
    t.stop().await;
}

#[tokio::test]
async fn a_sales_invoice_price_mirrors_the_vendor_invoice_price() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.purchase_order(&t, "10", "2.50").await;
    f.receive(&t, &order, "10").await;
    let sale = f.order(&t, "sales_order", "6", "9.00").await;
    f.deliver(&t, &sale, "6").await;

    // By default any price.
    let (status, body) = f.sales_invoice(&t, &sale, "2", "5.00").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    saved(
        &t,
        json!({ "company_id": f.c.id, "point": SALES_PRICE_DIFFERS, "effect": "refuse",
                "condition": "facts.difference_percent < -2",
                "message": "{invoice_price} is {difference_percent}% off the order price {order_price}" }),
    )
    .await;
    let (status, body) = f.sales_invoice(&t, &sale, "2", "8.00").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 8 is -11.111111% off the order price 9"
    );
    assert_eq!(body["error"]["field"], "unit_price");
    let (status, body) = f.sales_invoice(&t, &sale, "2", "8.90").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_company_can_refuse_posting_dates_it_does_not_expect() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.purchase_order(&t, "10", "2.50").await;
    // By default any date in an open period.
    let (status, receipt) = f.receive(&t, &order, "4").await;
    assert_eq!(status, StatusCode::OK, "{receipt}");

    // Asked for every posting of every module: here a receipt and a
    // journal entry, and reversals with a fact of their own.
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": POSTING_DATE, "effect": "refuse",
                "condition": "facts.days_from_today < -30 && facts.doc_type != 'purchase_receipt'",
                "message": "a {doc_type} dated {posting_date} is too old" }),
    )
    .await;
    saved(
        &t,
        json!({ "company_id": f.c.id, "point": POSTING_DATE, "effect": "refuse",
                "condition": "facts.reversal && facts.posting_date < '2026-04-01'",
                "message": "reversals are dated in the current month" }),
    )
    .await;
    let (status, body) = f.receive(&t, &order, "2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let draft = t.draft(&t.admin, &f.c, "1.00").await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "a journal_entry dated 2026-03-15 is too old"
    );
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_receipt.reverse",
            json!({ "id": id(&receipt) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), "reversals are dated in the current month");
    // Nothing of the refused postings remains.
    assert_eq!(f.on_hand(&t).await, 6.0);
    t.stop().await;
}

#[tokio::test]
async fn policy_facts_say_what_they_are_called_and_what_kind_of_number_they_are() {
    let t = TestApp::start().await;
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    let points = meta["policies"].as_array().unwrap();
    for point in points {
        let facts = point["facts"].as_array().unwrap();
        for fact in facts {
            assert!(
                fact["label"].as_str().is_some_and(|l| !l.is_empty()),
                "{}: {fact}",
                point["name"]
            );
            // Money in another fact's currency names a fact of the point.
            if fact["decimal"]["currency"]["from"] == "field" {
                let currency = &fact["decimal"]["currency"]["field"];
                assert!(
                    facts.iter().any(|f| &f["name"] == currency),
                    "{}: {} names a currency fact it does not have",
                    point["name"],
                    fact["name"]
                );
            }
            if fact["type"] == "number" && fact["name"].as_str().unwrap().ends_with("_percent") {
                assert_eq!(fact["decimal"]["role"], "percent", "{fact}");
            }
        }
    }
    let approve = points
        .iter()
        .find(|p| p["name"] == "purchase_order.approve")
        .unwrap();
    let total = approve["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "total_net")
        .unwrap();
    assert_eq!(total["label"], "Net total");
    assert_eq!(
        total["decimal"],
        json!({ "role": "money", "scale": 4,
                "currency": { "from": "field", "field": "currency" }, "unit": null })
    );
    let lines = approve["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "lines")
        .unwrap();
    assert_eq!(lines["decimal"], Value::Null);
    t.stop().await;
}
