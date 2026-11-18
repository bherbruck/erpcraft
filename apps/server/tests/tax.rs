//! Tax on invoices: worked out per line, posted, owed with the invoice,
//! and recorded for the tax report.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
    customer: String,
    account: BTreeMap<&'static str, String>,
    /// 19%, with accounts set up.
    vat: String,
}

async fn setup(t: &TestApp) -> Fixture {
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
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyer LLC" }))
        .await;
    let mut account = BTreeMap::new();
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("1400", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("2300", "liability"),
        ("5000", "expense"),
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
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "grni_account_id": account["2100"],
            "payable_account_id": account["2000"],
            "price_variance_account_id": account["5200"],
        }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({
            "company_id": c.id,
            "receivable_account_id": account["1200"],
            "revenue_account_id": c.revenue,
            "cogs_account_id": account["5000"],
        }),
    )
    .await;
    t.inventory_rule(&c.id, &account["1300"]).await;
    let vat = t
        .create(
            "tax_code",
            json!({ "code": "VAT19", "name": "VAT 19%", "rate": "19" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({
            "company_id": c.id,
            "tax_code_id": vat,
            "collected_account_id": account["2300"],
            "paid_account_id": account["1400"],
        }),
    )
    .await;
    Fixture {
        c,
        item,
        warehouse,
        vendor,
        customer,
        account,
        vat,
    }
}

async fn run(t: &TestApp, command: &str, doc: &Value) -> (StatusCode, Value) {
    t.command(&t.admin, command, json!({ "id": id(doc) })).await
}

impl Fixture {
    async fn create(&self, t: &TestApp, doc_type: &str, body: Value) -> Value {
        let (status, doc) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc}");
        doc
    }

    async fn done(&self, t: &TestApp, doc_type: &str, command: &str, body: Value) -> Value {
        let draft = self.create(t, doc_type, body).await;
        let (status, doc) = run(t, command, &draft).await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }

    /// An approved order of `kind` (`purchase` or `sales`) for one line,
    /// with the goods already received or delivered in full.
    async fn fulfilled(&self, t: &TestApp, kind: &str, qty: &str, price: &str) -> Value {
        let (party, step, moved) = match kind {
            "purchase" => (
                json!({ "vendor_id": self.vendor }),
                "purchase_receipt",
                "purchase_order_id",
            ),
            _ => (
                json!({ "customer_id": self.customer }),
                "sales_delivery",
                "sales_order_id",
            ),
        };
        let mut body = party;
        body["lines"] = json!([{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }]);
        let order = self
            .done(
                t,
                &format!("{kind}_order"),
                &format!("{kind}_order.approve"),
                body,
            )
            .await;
        self.done(
            t,
            step,
            &format!("{step}.post"),
            json!({ moved: order["id"], "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }] }),
        )
        .await;
        order
    }

    fn invoice_body(&self, kind: &str, order: &Value, qty: &str, price: &str, tax: Value) -> Value {
        let key = if kind == "purchase" {
            "purchase_order_id"
        } else {
            "sales_order_id"
        };
        json!({
            key: order["id"],
            "lines": [{
                "order_line_id": order["body"]["lines"][0]["id"],
                "qty": qty, "unit_price": price, "tax_code_id": tax,
            }],
        })
    }

    async fn balance(&self, t: &TestApp, code: &str) -> f64 {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["code"] == code)
            .map(|r| dec(&r["balance"]))
            .unwrap()
    }

    async fn report(&self, t: &TestApp, from: &str, to: &str) -> Value {
        let (status, report) = t
            .command(
                &t.admin,
                "tax.report",
                json!({ "company_id": self.c.id, "from": from, "to": to }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{report}");
        report
    }
}

#[tokio::test]
async fn tax_is_charged_posted_owed_and_reported() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Buy 10 at 2.00 plus 19%: the vendor is owed 23.80, of which 3.80 is
    // tax the company can reclaim. Stock is worth the net 20.00.
    let order = f.fulfilled(&t, "purchase", "10", "2.00").await;
    let bought = f
        .done(
            &t,
            "vendor_invoice",
            "vendor_invoice.post",
            f.invoice_body("purchase", &order, "10", "2.00", json!(f.vat)),
        )
        .await;
    assert_eq!(dec(&bought["body"]["lines"][0]["tax_amount"]), 3.8);
    assert_eq!(f.balance(&t, "2000").await, -23.8);
    assert_eq!(f.balance(&t, "1400").await, 3.8);
    assert_eq!(f.balance(&t, "1300").await, 20.0);
    assert_eq!(f.balance(&t, "2100").await, 0.0);

    // Sell 4 at 5.00 plus 19%: the customer owes 23.80, revenue is 20.00,
    // and 3.80 is owed to the tax authority.
    let order = f.fulfilled(&t, "sales", "4", "5.00").await;
    let sold = f
        .done(
            &t,
            "sales_invoice",
            "sales_invoice.post",
            f.invoice_body("sales", &order, "4", "5.00", json!(f.vat)),
        )
        .await;
    assert_eq!(f.balance(&t, "1200").await, 23.8);
    assert_eq!(f.balance(&t, "4000").await, -20.0);
    assert_eq!(f.balance(&t, "2300").await, -3.8);

    // What is owed includes the tax, so a payment settles the gross.
    let (_, due) = t
        .command(
            &t.admin,
            "open_item.list",
            json!({ "company_id": f.c.id, "kind": "receivable" }),
        )
        .await;
    assert_eq!(dec(&due["total_open"]), 23.8);
    f.done(
        &t,
        "payment",
        "payment.post",
        json!({
            "direction": "incoming", "bank_account_id": f.c.cash,
            "allocations": [{ "open_item_id": due["items"][0]["id"], "amount": "23.80" }],
        }),
    )
    .await;
    assert_eq!(f.balance(&t, "1200").await, 0.0);

    // The report for March.
    let report = f.report(&t, "2026-03-01", "2026-03-31").await;
    let row = &report["rows"][0];
    assert_eq!(row["code"], "VAT19");
    assert_eq!(dec(&row["collected_base"]), 20.0);
    assert_eq!(dec(&row["collected"]), 3.8);
    assert_eq!(dec(&row["paid_base"]), 20.0);
    assert_eq!(dec(&row["paid"]), 3.8);
    assert_eq!(dec(&report["net"]), 0.0);
    assert_eq!(
        f.report(&t, "2026-04-01", "2026-04-30").await["rows"],
        json!([])
    );

    // Reversing the purchase invoice in April takes the tax back in April,
    // and leaves March as it was reported.
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.reverse",
            json!({ "id": id(&bought), "posting_date": "2026-04-02" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.balance(&t, "1400").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, 0.0);
    let april = f.report(&t, "2026-04-01", "2026-04-30").await;
    assert_eq!(dec(&april["rows"][0]["paid"]), -3.8);
    assert_eq!(dec(&april["net"]), 3.8);
    let march = f.report(&t, "2026-03-01", "2026-03-31").await;
    assert_eq!(dec(&march["rows"][0]["paid"]), 3.8);
    // The sales invoice was paid, so it stays.
    let (status, _) = run(&t, "sales_invoice.reverse", &sold).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The reversing entries are in recording order: after everything
    // recorded before them, with ids ordered by time like every other.
    assert_eq!(
        t.count(
            "SELECT count(*) FROM tax_entry r WHERE r.reverses_id IS NOT NULL AND (
                substring(r.id::text, 15, 1) <> '7'
                OR r.id < ANY (SELECT e.id FROM tax_entry e WHERE e.reverses_id IS NULL))"
        )
        .await,
        0
    );

    // The record cannot be rewritten.
    for sql in ["UPDATE tax_entry SET amount = 0", "DELETE FROM tax_entry"] {
        assert!(sqlx::query(sql).execute(&t.pool).await.is_err(), "{sql}");
    }

    t.stop().await;
}

#[tokio::test]
async fn sales_tax_that_cannot_be_reclaimed_is_an_expense() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // A sales tax of 7.25%: collected from customers, and when a vendor
    // charges it, it is a cost.
    let sales_tax = t
        .create(
            "tax_code",
            json!({ "code": "OH-CUY", "name": "Ohio, Cuyahoga County", "rate": "7.25" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({
            "company_id": f.c.id,
            "tax_code_id": sales_tax,
            "collected_account_id": f.account["2300"],
            "paid_account_id": f.account["5200"],
        }),
    )
    .await;

    let order = f.fulfilled(&t, "purchase", "3", "9.99").await;
    f.done(
        &t,
        "vendor_invoice",
        "vendor_invoice.post",
        f.invoice_body("purchase", &order, "3", "9.99", json!(sales_tax)),
    )
    .await;
    // 29.97 net; 7.25% is 2.172825, rounded to 2.17.
    assert_eq!(f.balance(&t, "2000").await, -32.14);
    assert_eq!(f.balance(&t, "5200").await, 2.17);
    assert_eq!(f.balance(&t, "1300").await, 29.97);

    // An invoice line without a tax code is not taxed.
    let order = f.fulfilled(&t, "sales", "1", "10.00").await;
    let sold = f
        .done(
            &t,
            "sales_invoice",
            "sales_invoice.post",
            f.invoice_body("sales", &order, "1", "10.00", Value::Null),
        )
        .await;
    assert!(sold["body"]["lines"][0]["tax_amount"].is_null());
    assert_eq!(f.balance(&t, "1200").await, 10.0);
    assert_eq!(f.balance(&t, "2300").await, 0.0);

    t.stop().await;
}

#[tokio::test]
async fn a_tax_code_must_be_usable_before_an_invoice_posts() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.fulfilled(&t, "purchase", "5", "1.00").await;

    // A code the company has no accounts for.
    let unset = t
        .create(
            "tax_code",
            json!({ "code": "GST5", "name": "GST 5%", "rate": "5" }),
        )
        .await;
    let draft = f
        .create(
            &t,
            "vendor_invoice",
            f.invoice_body("purchase", &order, "5", "1.00", json!(unset)),
        )
        .await;
    let (status, body) = run(&t, "vendor_invoice.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "this company has not said which accounts tax code GST5 posts to"
    );
    // Nothing of the refused invoice remains.
    assert_eq!(f.balance(&t, "2000").await, 0.0);
    assert_eq!(t.count("SELECT count(*) FROM tax_entry").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM open_item").await, 0);

    // An inactive code.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/tax_code/{}", f.vat),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let draft = f
        .create(
            &t,
            "vendor_invoice",
            f.invoice_body("purchase", &order, "5", "1.00", json!(f.vat)),
        )
        .await;
    let (status, body) = run(&t, "vendor_invoice.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["message"], "tax code VAT19 is inactive");

    // Tax accounts must be the company's own, and a rate is a percentage.
    let other = t.company("OTHER", "USD").await;
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/tax_settings",
            json!({
                "company_id": other.id, "tax_code_id": unset,
                "collected_account_id": f.account["2300"], "paid_account_id": other.cash,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/tax_code",
            json!({ "code": "BAD", "name": "Bad", "rate": "190" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // The report has its own permission.
    let reader = t
        .user_with_role("reader@example.com", &["account:read"], &f.c.id)
        .await;
    let (status, _) = t
        .command(
            &reader,
            "tax.report",
            json!({ "company_id": f.c.id, "from": "2026-01-01", "to": "2026-12-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .command(
            &t.admin,
            "tax.report",
            json!({ "company_id": f.c.id, "from": "2026-12-31", "to": "2026-01-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    t.stop().await;
}

#[tokio::test]
async fn a_company_can_still_invoice_at_a_retired_code() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.fulfilled(&t, "purchase", "5", "1.00").await;
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/tax_code/{}", f.vat),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let draft = f
        .create(
            &t,
            "vendor_invoice",
            f.invoice_body("purchase", &order, "5", "1.00", json!(f.vat)),
        )
        .await;
    let (status, body) = run(&t, "vendor_invoice.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["message"], "tax code VAT19 is inactive");

    // The company's own words, then an exception for vendor invoices.
    let rule = |data: Value| {
        let t = &t;
        async move {
            let (status, body) = t.post(&t.admin, "/api/entities/policy_rule", data).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            body["id"].as_str().unwrap().to_owned()
        }
    };
    let words = rule(json!({
        "company_id": f.c.id, "point": "tax_code.use", "effect": "refuse",
        "condition": "!facts.active", "message": "{code} at {rate}% is retired", "priority": 1,
    }))
    .await;
    let (status, body) = run(&t, "vendor_invoice.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["message"], "VAT19 at 19% is retired");
    t.patch(
        &t.admin,
        &format!("/api/entities/policy_rule/{words}"),
        json!({ "active": false }),
    )
    .await;
    rule(json!({
        "company_id": f.c.id, "point": "tax_code.use", "effect": "allow",
        "condition": "facts.code == 'VAT19' && facts.direction == 'paid'",
    }))
    .await;
    let (status, body) = run(&t, "vendor_invoice.post", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(t.count("SELECT count(*) FROM tax_entry").await, 1);
    t.stop().await;
}
