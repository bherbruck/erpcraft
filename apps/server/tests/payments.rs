//! Open items and payments: invoices become debts, payments settle them.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
    customer: String,
}

/// A company set up to buy, stock and sell widgets.
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
    let mut account = std::collections::BTreeMap::new();
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
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
    Fixture {
        c,
        item,
        warehouse,
        vendor,
        customer,
    }
}

impl Fixture {
    async fn create(&self, t: &TestApp, doc_type: &str, body: Value) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": self.c.id, "doc_date": "2026-03-15", "body": body }),
        )
        .await
    }

    /// Draft a document and run `command` on it; both must succeed.
    async fn done(&self, t: &TestApp, doc_type: &str, command: &str, body: Value) -> Value {
        let (status, draft) = self.create(t, doc_type, body).await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, doc) = run(t, command, &draft).await;
        assert_eq!(status, StatusCode::OK, "{doc}");
        doc
    }

    /// Buy `qty` widgets at `price`: order, receive and invoice in full.
    /// Returns the posted vendor invoice.
    async fn buy(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let order = self
            .done(
                t,
                "purchase_order",
                "purchase_order.approve",
                json!({
                    "vendor_id": self.vendor,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
                }),
            )
            .await;
        let line = &order["body"]["lines"][0]["id"];
        self.done(
            t,
            "purchase_receipt",
            "purchase_receipt.post",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        self.done(
            t,
            "vendor_invoice",
            "vendor_invoice.post",
            json!({
                "purchase_order_id": order["id"],
                "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price }],
            }),
        )
        .await
    }

    /// Sell `qty` widgets at `price`: order, deliver and invoice in full.
    /// Returns the posted sales invoice.
    async fn sell(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let order = self
            .done(
                t,
                "sales_order",
                "sales_order.approve",
                json!({
                    "customer_id": self.customer,
                    "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
                }),
            )
            .await;
        let line = &order["body"]["lines"][0]["id"];
        self.done(
            t,
            "sales_delivery",
            "sales_delivery.post",
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        self.done(
            t,
            "sales_invoice",
            "sales_invoice.post",
            json!({
                "sales_order_id": order["id"],
                "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price }],
            }),
        )
        .await
    }

    async fn open_items(&self, t: &TestApp, filter: Value) -> Value {
        let mut input = json!({ "company_id": self.c.id });
        for (key, value) in filter.as_object().unwrap() {
            input[key] = value.clone();
        }
        let (status, list) = t.command(&t.admin, "open_item.list", input).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        list
    }

    fn payment(&self, direction: &str, allocations: Value) -> Value {
        json!({ "direction": direction, "bank_account_id": self.c.cash, "allocations": allocations })
    }

    /// Balance of one account, debit minus credit.
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
}

async fn run(t: &TestApp, command: &str, doc: &Value) -> (StatusCode, Value) {
    t.command(&t.admin, command, json!({ "id": id(doc) })).await
}

#[tokio::test]
async fn a_vendor_invoice_is_owed_until_it_is_paid() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let invoice = f.buy(&t, "4", "2.75").await;

    // The invoice is now a debt of 11.00 to the vendor.
    let owed = f.open_items(&t, json!({ "kind": "payable" })).await;
    assert_eq!(owed["total"], 1);
    assert_eq!(dec(&owed["total_open"]), 11.0);
    let item = &owed["items"][0];
    assert_eq!(item["party_type"], "vendor");
    assert_eq!(item["party_id"], f.vendor);
    assert_eq!(item["document_id"], invoice["id"]);
    assert_eq!(item["number"], "VI-2026-00001");
    assert_eq!(dec(&item["open"]), 11.0);
    assert_eq!(f.balance(&t, "2000").await, -11.0);

    // Part of it is paid from the bank.
    let part = f
        .done(
            &t,
            "payment",
            "payment.post",
            f.payment(
                "outgoing",
                json!([{ "open_item_id": item["id"], "amount": "5.00" }]),
            ),
        )
        .await;
    assert_eq!(part["number"], "PAY-2026-00001");
    assert_eq!(dec(&part["body"]["amount"]), 5.0);
    assert_eq!(f.balance(&t, "2000").await, -6.0);
    assert_eq!(f.balance(&t, "1000").await, -5.0);
    let owed = f.open_items(&t, json!({})).await;
    assert_eq!(dec(&owed["items"][0]["open"]), 6.0);

    // Never more than is open.
    let (status, draft) = f
        .create(
            &t,
            "payment",
            f.payment(
                "outgoing",
                json!([{ "open_item_id": item["id"], "amount": "6.01" }]),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = run(&t, "payment.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        "line 1: only 6 is still open on this item, not 6.01"
    );

    // An invoice that has been paid, even in part, cannot be reversed.
    let (status, body) = run(&t, "vendor_invoice.reverse", &invoice).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reverse the payment first"),
        "{body}"
    );
    assert_eq!(
        f.balance(&t, "2000").await,
        -6.0,
        "the refused reversal left nothing"
    );

    // The rest is paid; nothing is owed.
    f.done(
        &t,
        "payment",
        "payment.post",
        f.payment(
            "outgoing",
            json!([{ "open_item_id": item["id"], "amount": "6.00" }]),
        ),
    )
    .await;
    assert_eq!(f.balance(&t, "2000").await, 0.0);
    assert_eq!(f.open_items(&t, json!({})).await["total"], 0);
    let all = f.open_items(&t, json!({ "include_settled": true })).await;
    assert_eq!(dec(&all["items"][0]["settled"]), 11.0);

    // The first payment bounces: 5.00 is owed again.
    let (status, body) = run(&t, "payment.reverse", &part).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "reversed");
    assert_eq!(f.balance(&t, "2000").await, -5.0);
    assert_eq!(f.balance(&t, "1000").await, -6.0);
    assert_eq!(dec(&f.open_items(&t, json!({})).await["total_open"]), 5.0);
    // Reversing it again changes nothing.
    let (status, _) = run(&t, "payment.reverse", &part).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dec(&f.open_items(&t, json!({})).await["total_open"]), 5.0);

    t.stop().await;
}

#[tokio::test]
async fn a_reversed_invoice_is_no_longer_owed() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let invoice = f.buy(&t, "4", "2.50").await;
    assert_eq!(dec(&f.open_items(&t, json!({})).await["total_open"]), 10.0);

    let (status, body) = run(&t, "vendor_invoice.reverse", &invoice).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.open_items(&t, json!({})).await["total"], 0);
    assert_eq!(f.balance(&t, "2000").await, 0.0);

    // It cannot be paid any more.
    let all = f.open_items(&t, json!({ "include_settled": true })).await;
    let (status, draft) = f
        .create(
            &t,
            "payment",
            f.payment(
                "outgoing",
                json!([{ "open_item_id": all["items"][0]["id"], "amount": "1.00" }]),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = run(&t, "payment.post", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_customer_pays_several_invoices_at_once() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.buy(&t, "10", "2.00").await;
    f.sell(&t, "3", "5.00").await;
    f.sell(&t, "2", "5.00").await;

    let due = f
        .open_items(
            &t,
            json!({ "kind": "receivable", "party_type": "customer", "party_id": f.customer }),
        )
        .await;
    assert_eq!(due["total"], 2);
    assert_eq!(dec(&due["total_open"]), 25.0);
    assert_eq!(f.balance(&t, "1200").await, 25.0);
    let (first, second) = (&due["items"][0], &due["items"][1]);

    // One payment of 20.00: the first invoice in full, half of the second.
    let payment = f
        .done(
            &t,
            "payment",
            "payment.post",
            f.payment(
                "incoming",
                json!([
                    { "open_item_id": first["id"], "amount": "15.00" },
                    { "open_item_id": second["id"], "amount": "5.00" },
                ]),
            ),
        )
        .await;
    assert_eq!(dec(&payment["body"]["amount"]), 20.0);
    assert_eq!(f.balance(&t, "1200").await, 5.0);
    // Bank: 20.00 in, and the purchase has not been paid yet.
    assert_eq!(f.balance(&t, "1000").await, 20.0);
    let due = f.open_items(&t, json!({ "kind": "receivable" })).await;
    assert_eq!(due["total"], 1);
    assert_eq!(due["items"][0]["id"], second["id"]);
    assert_eq!(dec(&due["items"][0]["open"]), 5.0);

    // The vendor is still owed, separately.
    let owed = f.open_items(&t, json!({ "kind": "payable" })).await;
    assert_eq!(dec(&owed["total_open"]), 20.0);

    t.stop().await;
}

#[tokio::test]
async fn a_payment_is_checked_before_it_moves_money() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.buy(&t, "10", "2.00").await;
    f.sell(&t, "1", "5.00").await;
    let payable = f.open_items(&t, json!({ "kind": "payable" })).await["items"][0]["id"].clone();
    let receivable =
        f.open_items(&t, json!({ "kind": "receivable" })).await["items"][0]["id"].clone();

    // The direction must fit what is settled, an item appears once, the
    // amount is more than zero, and the bank account is the company's.
    let other = t.company("OTHER", "USD").await;
    for (body, why) in [
        (
            f.payment(
                "outgoing",
                json!([{ "open_item_id": receivable, "amount": "1" }]),
            ),
            "settles payables",
        ),
        (
            f.payment(
                "incoming",
                json!([{ "open_item_id": payable, "amount": "1" }]),
            ),
            "settles receivables",
        ),
        (
            f.payment(
                "outgoing",
                json!([
                    { "open_item_id": payable, "amount": "1" },
                    { "open_item_id": payable, "amount": "1" },
                ]),
            ),
            "only once",
        ),
        (
            f.payment(
                "outgoing",
                json!([{ "open_item_id": payable, "amount": "0" }]),
            ),
            "more than zero",
        ),
        (
            json!({ "direction": "outgoing", "bank_account_id": other.cash, "allocations": [] }),
            "must belong",
        ),
    ] {
        let (status, body) = f.create(&t, "payment", body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(why),
            "{why}: {body}"
        );
    }

    // An empty payment cannot be posted.
    let (_, empty) = f
        .create(&t, "payment", f.payment("outgoing", json!([])))
        .await;
    let (status, body) = run(&t, "payment.post", &empty).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Two payments of the whole amount at the same moment: one is refused.
    let pay = || {
        let (t, f, payable) = (&t, &f, payable.clone());
        async move {
            let (status, draft) = f
                .create(
                    t,
                    "payment",
                    f.payment(
                        "outgoing",
                        json!([{ "open_item_id": payable, "amount": "20.00" }]),
                    ),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            run(t, "payment.post", &draft).await
        }
    };
    let (a, b) = tokio::join!(pay(), pay());
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY],
        "{} / {}",
        a.1,
        b.1
    );
    assert_eq!(f.balance(&t, "2000").await, 0.0);
    assert_eq!(f.balance(&t, "1000").await, -20.0);

    // The detail cannot be changed behind the ledger's back.
    for sql in [
        "UPDATE open_item SET settled = 0",
        "UPDATE open_item SET amount = 1",
        "DELETE FROM open_item",
        "UPDATE settlement SET amount = 1",
        "DELETE FROM settlement",
    ] {
        assert!(sqlx::query(sql).execute(&t.pool).await.is_err(), "{sql}");
    }

    // Seeing what is owed is a permission like any other.
    let reader = t
        .user_with_role("reader@example.com", &["account:read"], &f.c.id)
        .await;
    let (status, _) = t
        .command(&reader, "open_item.list", json!({ "company_id": f.c.id }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    t.stop().await;
}

#[tokio::test]
async fn open_items_always_add_up_to_the_control_accounts() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.buy(&t, "10", "2.00").await;
    let second = f.buy(&t, "5", "3.00").await;
    f.sell(&t, "4", "6.00").await;
    let payable = f.open_items(&t, json!({ "kind": "payable" })).await;
    f.done(
        &t,
        "payment",
        "payment.post",
        f.payment(
            "outgoing",
            json!([{ "open_item_id": payable["items"][0]["id"], "amount": "7.50" }]),
        ),
    )
    .await;
    let (status, body) = run(&t, "vendor_invoice.reverse", &second).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Whatever happened, the list of what is open equals the accounts.
    let owed = dec(&f.open_items(&t, json!({ "kind": "payable" })).await["total_open"]);
    let due = dec(&f.open_items(&t, json!({ "kind": "receivable" })).await["total_open"]);
    assert_eq!(owed, 12.5);
    assert_eq!(-f.balance(&t, "2000").await, owed);
    assert_eq!(f.balance(&t, "1200").await, due);

    t.stop().await;
}

#[tokio::test]
async fn open_items_are_listed_by_days_past_due() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.buy(&t, "10", "2.00").await;
    f.sell(&t, "3", "5.00").await;

    // Both invoices are dated 2026-03-15 and have no terms: they are due
    // that day, so on 2026-04-20 they are 36 days past due.
    let count = |filter: Value| {
        let f = &f;
        let t = &t;
        async move { f.open_items(t, filter).await["total"].clone() }
    };
    let late = json!({ "as_of": "2026-04-20" });
    let with = |extra: Value| {
        let mut input = late.clone();
        for (key, value) in extra.as_object().unwrap() {
            input[key] = value.clone();
        }
        input
    };
    assert_eq!(
        count(with(json!({ "overdue_min": 31, "overdue_max": 60 }))).await,
        2
    );
    assert_eq!(
        count(with(json!({ "overdue_min": 36, "overdue_max": 36 }))).await,
        2
    );
    assert_eq!(count(with(json!({ "overdue_min": 37 }))).await, 0);
    assert_eq!(count(with(json!({ "overdue_max": 35 }))).await, 0);
    assert_eq!(
        count(with(json!({ "overdue_min": 1, "kind": "receivable" }))).await,
        1
    );
    // On the due day nothing is past due yet: all of it is current.
    assert_eq!(
        count(json!({ "as_of": "2026-03-15", "overdue_max": 0 })).await,
        2
    );
    assert_eq!(
        count(json!({ "as_of": "2026-03-15", "overdue_min": 1 })).await,
        0
    );

    let (status, body) = t
        .command(
            &t.admin,
            "open_item.list",
            json!({ "company_id": f.c.id, "overdue_min": 60, "overdue_max": 30 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}
