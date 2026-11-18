//! Returns and credit notes: goods back to a vendor or from a customer,
//! and the credit for them, with the books checked at every step: the
//! trial balance balances, the inventory account equals the value of the
//! stock, open items add up to the payables and receivables accounts, and
//! goods received not invoiced clears.

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

const MARCH: &str = "2026-03-15";
const APRIL: &str = "2026-04-10";

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
        ("5300", "expense"),
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
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "stock_variance", "account_id": account["5300"] }),
    )
    .await;
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

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

/// Run a command on a document; returns the answer.
async fn run(t: &TestApp, command: &str, doc: &Value) -> (StatusCode, Value) {
    t.command(&t.admin, command, json!({ "id": id(doc) })).await
}

/// Run a command on a document that must succeed.
async fn done(t: &TestApp, command: &str, doc: &Value) -> Value {
    let (status, body) = run(t, command, doc).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

impl Fixture {
    /// Draft a document; returns the answer.
    async fn draft(
        &self,
        t: &TestApp,
        doc_type: &str,
        date: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        t.post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": self.c.id, "doc_date": date, "body": body }),
        )
        .await
    }

    /// Draft a document and post it; it must post.
    async fn posted(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = self.draft(t, doc_type, date, body).await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        done(t, &format!("{doc_type}.post"), &draft).await
    }

    /// Draft a document and try to post it.
    async fn try_post(
        &self,
        t: &TestApp,
        doc_type: &str,
        date: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, draft) = self.draft(t, doc_type, date, body).await;
        if status != StatusCode::CREATED {
            return (status, draft);
        }
        run(t, &format!("{doc_type}.post"), &draft).await
    }

    /// An approved order of `kind` (`purchase` or `sales`) for widgets.
    async fn order(&self, t: &TestApp, kind: &str, qty: &str, price: &str) -> Value {
        let mut body = match kind {
            "purchase" => json!({ "vendor_id": self.vendor }),
            _ => json!({ "customer_id": self.customer }),
        };
        body["lines"] = json!([{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }]);
        let doc_type = format!("{kind}_order");
        let (status, draft) = self.draft(t, &doc_type, MARCH, body).await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        done(t, &format!("{doc_type}.approve"), &draft).await
    }

    fn line(order: &Value) -> &Value {
        &order["body"]["lines"][0]["id"]
    }

    async fn receive(&self, t: &TestApp, order: &Value, qty: &str) -> Value {
        self.posted(
            t,
            "purchase_receipt",
            MARCH,
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": Self::line(order), "qty": qty }] }),
        )
        .await
    }

    async fn vendor_invoice(&self, t: &TestApp, order: &Value, qty: &str, price: &str) -> Value {
        self.posted(
            t,
            "vendor_invoice",
            MARCH,
            json!({
                "purchase_order_id": order["id"],
                "lines": [{ "order_line_id": Self::line(order), "qty": qty, "unit_price": price, "tax_code_id": self.vat }],
            }),
        )
        .await
    }

    fn return_body(&self, order: &Value, qty: &str) -> Value {
        let key = if order["body"]["vendor_id"].is_string() {
            "purchase_order_id"
        } else {
            "sales_order_id"
        };
        json!({ key: order["id"], "lines": [{ "order_line_id": Self::line(order), "qty": qty }] })
    }

    /// The balance of an account; on the way, the books are checked:
    /// they balance, the inventory account equals the value of the stock,
    /// and open items equal the payables and receivables accounts.
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
        for (kind, account, sign) in [("payable", "2000", -1.0), ("receivable", "1200", 1.0)] {
            let (status, items) = t
                .command(
                    &t.admin,
                    "open_item.list",
                    json!({ "company_id": self.c.id, "kind": kind }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{items}");
            assert!(
                (dec(&items["total_open"]) - sign * balance(account)).abs() < 1e-9,
                "open {kind}s {} and account {account} {} agree",
                items["total_open"],
                balance(account)
            );
        }
        balance(code)
    }

    async fn on_hand(&self, t: &TestApp) -> (f64, f64) {
        self.balance(t, "1300").await;
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
            .map_or((0.0, 0.0), |r| (dec(&r["qty"]), dec(&r["value"])))
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

    /// The open items of a kind, oldest first.
    async fn items(&self, t: &TestApp, kind: &str) -> Vec<Value> {
        let (_, items) = t
            .command(
                &t.admin,
                "open_item.list",
                json!({ "company_id": self.c.id, "kind": kind }),
            )
            .await;
        items["items"].as_array().unwrap().clone()
    }

    async fn now(&self, t: &TestApp, doc_type: &str, doc: &Value) -> Value {
        let (status, body) = t
            .get(&t.admin, &format!("/api/documents/{doc_type}/{}", id(doc)))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

#[tokio::test]
async fn goods_go_back_to_the_vendor_and_are_credited() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Ten bought at 2.00, received and invoiced with 19% tax.
    let order = f.order(&t, "purchase", "10", "2.00").await;
    f.receive(&t, &order, "10").await;
    let invoice = f.vendor_invoice(&t, &order, "10", "2.00").await;
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -23.8);
    assert_eq!(
        f.now(&t, "purchase_order", &order).await["status"],
        "closed"
    );

    // Three go back in April. They leave stock at what they cost, and wait
    // in GRNI, now a debit, for the vendor's credit.
    let (status, draft) = run(&t, "purchase_order.return", &order).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["qty"]), 10.0);
    let (status, body) = f
        .try_post(&t, "purchase_return", APRIL, f.return_body(&order, "11"))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 10 have been received and not returned on order line 1, not 11"
    );
    let ret = f
        .posted(&t, "purchase_return", APRIL, f.return_body(&order, "3"))
        .await;
    assert_eq!(dec(&ret["body"]["lines"][0]["value"]), 6.0);
    assert_eq!(dec(&ret["summary"]["total_value"]), 6.0);
    assert_eq!(f.on_hand(&t).await, (7.0, 14.0));
    assert_eq!(f.balance(&t, "2100").await, 6.0);
    assert_eq!(f.balance(&t, "5200").await, 0.0);
    // The order is complete no longer, but stays closed by default.
    let now = f.now(&t, "purchase_order", &order).await;
    assert_eq!(now["status"], "closed");
    assert_eq!(dec(&now["body"]["lines"][0]["qty_received"]), 7.0);
    assert_eq!(dec(&now["body"]["lines"][0]["qty_returned"]), 3.0);

    // The vendor credits the three, with the tax it charged on them.
    let (status, credit) = t
        .command(
            &t.admin,
            "vendor_invoice.credit",
            json!({ "id": invoice["id"], "doc_date": APRIL }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    let line = &credit["body"]["lines"][0];
    assert_eq!(dec(&line["qty"]), 3.0);
    assert_eq!(dec(&line["unit_price"]), 2.0);
    assert_eq!(line["tax_code_id"], json!(f.vat));
    assert_eq!(dec(&credit["summary"]["total_gross"]), 7.14);
    let credit = done(&t, "vendor_credit_note.post", &credit).await;
    assert_eq!(credit["status"], "posted");
    assert_eq!(dec(&credit["body"]["lines"][0]["tax_amount"]), 1.14);
    assert_eq!(dec(&credit["body"]["lines"][0]["returned_qty"]), 3.0);
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -16.66);
    assert_eq!(f.balance(&t, "1400").await, 2.66);
    let items = f.items(&t, "payable").await;
    assert_eq!(items.len(), 2);
    assert_eq!(dec(&items[1]["open"]), -7.14);
    assert_eq!(items[1]["doc_type"], "vendor_credit_note");

    // March, already reported, still reads as it did; the credit is in
    // April's return.
    let march = f.report(&t, "2026-03-01", "2026-03-31").await;
    assert_eq!(dec(&march["total_paid"]), 3.8);
    let april = f.report(&t, "2026-04-01", "2026-04-30").await;
    assert_eq!(dec(&april["total_paid"]), -1.14);
    assert_eq!(dec(&april["rows"][0]["paid_base"]), -6.0);

    // Nothing more waits for a credit, and a credit without a return is
    // refused by default.
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.credit",
            json!({ "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let invoice_line = &invoice["body"]["lines"][0]["id"];
    let price_only = json!({
        "vendor_invoice_id": invoice["id"],
        "lines": [{ "invoice_line_id": invoice_line, "qty": "1", "unit_price": "0.50" }],
    });
    let (status, body) = f
        .try_post(&t, "vendor_credit_note", APRIL, price_only.clone())
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 0 of the goods on order line 1 have been returned and not credited, not 1; a credit without a return is not accepted"
    );

    // What was credited cannot be undone underneath the credit.
    let (status, body) = run(&t, "purchase_return.reverse", &ret).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        message(&body).contains("reverse the credit note first"),
        "{body}"
    );
    let (status, body) = run(&t, "vendor_invoice.reverse", &invoice).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        message(&body).contains("reverse the credit note first"),
        "{body}"
    );

    // One payment settles the invoice and the credit together: 16.66.
    let ids: Vec<Value> = items.iter().map(|i| i["id"].clone()).collect();
    let (status, payment) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": f.c.id, "open_item_ids": ids, "bank_account_id": f.c.cash, "doc_date": APRIL }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{payment}");
    assert_eq!(dec(&payment["body"]["amount"]), 16.66);
    let payment = done(&t, "payment.post", &payment).await;
    assert_eq!(dec(&payment["summary"]["total"]), 16.66);
    assert_eq!(f.balance(&t, "2000").await, 0.0);
    assert_eq!(f.balance(&t, "1000").await, -16.66);
    assert!(f.items(&t, "payable").await.is_empty());

    // Reversing the payment opens both again.
    done(&t, "payment.reverse", &payment).await;
    assert_eq!(f.items(&t, "payable").await.len(), 2);
    assert_eq!(f.balance(&t, "2000").await, -16.66);
    t.stop().await;
}

#[tokio::test]
async fn a_return_before_the_invoice_leaves_less_to_invoice() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "10", "2.50").await;
    f.receive(&t, &order, "10").await;
    // Two go back before the vendor invoices: GRNI holds the other eight.
    let ret = f
        .posted(&t, "purchase_return", MARCH, f.return_body(&order, "2"))
        .await;
    assert_eq!(f.balance(&t, "2100").await, -20.0);
    assert_eq!(f.on_hand(&t).await, (8.0, 20.0));
    // Only the eight kept can be invoiced, and that clears GRNI and
    // completes the order.
    let (status, draft) = run(&t, "purchase_order.invoice", &order).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["qty"]), 8.0);
    let (status, body) = f
        .try_post(
            &t,
            "vendor_invoice",
            MARCH,
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": Fixture::line(&order), "qty": "9", "unit_price": "2.50" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let invoice = f.vendor_invoice(&t, &order, "8", "2.50").await;
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -23.8);
    // Eight of ten kept and invoiced: the order is not complete, so open.
    assert_eq!(f.now(&t, "purchase_order", &order).await["status"], "open");

    // Nothing invoiced was returned, so the return can be taken back once
    // the invoice is; the order is as it was after the receipt.
    let (status, body) = run(&t, "purchase_return.reverse", &ret).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.on_hand(&t).await, (10.0, 25.0));
    assert_eq!(f.balance(&t, "2100").await, -5.0);
    done(&t, "vendor_invoice.reverse", &invoice).await;
    assert_eq!(f.balance(&t, "2100").await, -25.0);
    assert_eq!(f.balance(&t, "2000").await, 0.0);
    t.stop().await;
}

#[tokio::test]
async fn a_credit_can_be_applied_reversed_and_allowed_for_price_only() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "10", "2.00").await;
    f.receive(&t, &order, "10").await;
    let invoice = f.vendor_invoice(&t, &order, "10", "2.00").await;
    let ret = f
        .posted(&t, "purchase_return", MARCH, f.return_body(&order, "4"))
        .await;
    let invoice_line = invoice["body"]["lines"][0]["id"].clone();

    // A partial credit, then the rest.
    let first = f
        .posted(
            &t,
            "vendor_credit_note",
            MARCH,
            json!({ "vendor_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "1" }] }),
        )
        .await;
    assert_eq!(f.balance(&t, "2100").await, 6.0);
    let second = f
        .posted(
            &t,
            "vendor_credit_note",
            MARCH,
            json!({ "vendor_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "3" }] }),
        )
        .await;
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "1400").await, 2.28);
    assert_eq!(f.balance(&t, "2000").await, -14.28);

    // The first credit is applied to the invoice without money moving.
    let items = f.items(&t, "payable").await;
    let invoice_item = items
        .iter()
        .find(|i| i["doc_type"] == "vendor_invoice")
        .unwrap();
    let credit_item = items
        .iter()
        .find(|i| i["document_id"] == first["id"])
        .unwrap();
    let (status, applied) = t
        .command(
            &t.admin,
            "payment.apply_credit",
            json!({ "company_id": f.c.id, "credit_item_id": credit_item["id"], "debt_item_id": invoice_item["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["status"], "posted");
    assert_eq!(dec(&applied["summary"]["total"]), 0.0);
    assert_eq!(f.balance(&t, "1000").await, 0.0);
    let items = f.items(&t, "payable").await;
    assert_eq!(items.len(), 2, "the first credit is used up");
    assert_eq!(dec(&items[0]["open"]), 21.42);
    // An applied credit cannot be reversed underneath; the other can.
    let (status, body) = run(&t, "vendor_credit_note.reverse", &first).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).contains("reverse the payment first"),
        "{body}"
    );
    done(&t, "vendor_credit_note.reverse", &second).await;
    assert_eq!(f.balance(&t, "2100").await, 6.0);
    let (status, body) = run(&t, "purchase_return.reverse", &ret).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    done(&t, "payment.reverse", &applied).await;
    done(&t, "vendor_credit_note.reverse", &first).await;
    done(&t, "purchase_return.reverse", &ret).await;
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -23.8);
    assert_eq!(f.on_hand(&t).await, (10.0, 20.0));

    // A company that accepts allowances lets a credit for price only
    // through: it is all price variance.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "vendor_credit_note.without_return", "effect": "allow", "condition": null }),
    )
    .await;
    f.posted(
        &t,
        "vendor_credit_note",
        MARCH,
        json!({ "vendor_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "10", "unit_price": "0.10" }] }),
    )
    .await;
    assert_eq!(f.balance(&t, "5200").await, -1.0);
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(f.balance(&t, "2000").await, -22.61);
    // The invoice line is credited in full now; nothing more.
    let (status, body) = f
        .try_post(
            &t,
            "vendor_credit_note",
            MARCH,
            json!({ "vendor_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_returned_order_reopens_for_replacements_where_the_company_wants_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "purchase_order.reopen_on_return", "effect": "allow", "condition": null }),
    )
    .await;
    let order = f.order(&t, "purchase", "5", "2.00").await;
    f.receive(&t, &order, "5").await;
    f.vendor_invoice(&t, &order, "5", "2.00").await;
    assert_eq!(
        f.now(&t, "purchase_order", &order).await["status"],
        "closed"
    );
    // Two go back, not yet credited; the order reopens and the vendor
    // sends two replacements, which level the line again without a credit.
    f.posted(&t, "purchase_return", MARCH, f.return_body(&order, "2"))
        .await;
    assert_eq!(f.now(&t, "purchase_order", &order).await["status"], "open");
    assert_eq!(f.balance(&t, "2100").await, 4.0);
    f.receive(&t, &order, "2").await;
    assert_eq!(f.balance(&t, "2100").await, 0.0);
    assert_eq!(
        f.now(&t, "purchase_order", &order).await["status"],
        "closed"
    );
    assert_eq!(f.on_hand(&t).await, (5.0, 10.0));
    t.stop().await;
}

#[tokio::test]
async fn money_paid_on_account_is_a_credit_to_apply_later() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // 50.00 paid to the vendor before any invoice.
    let (status, payment) = f
        .draft(
            &t,
            "payment",
            MARCH,
            json!({
                "direction": "outgoing", "bank_account_id": f.c.cash, "on_account": "50.00",
                "party_type": "vendor", "party_id": f.vendor, "account_id": f.account["2000"],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{payment}");
    let payment = done(&t, "payment.post", &payment).await;
    assert_eq!(dec(&payment["summary"]["total"]), 50.0);
    assert_eq!(f.balance(&t, "2000").await, 50.0);
    let items = f.items(&t, "payable").await;
    assert_eq!(dec(&items[0]["open"]), -50.0);

    // The invoice comes; the money on account is applied to it.
    let order = f.order(&t, "purchase", "10", "2.00").await;
    f.receive(&t, &order, "10").await;
    f.vendor_invoice(&t, &order, "10", "2.00").await;
    let items = f.items(&t, "payable").await;
    let (status, applied) = t
        .command(
            &t.admin,
            "payment.apply_credit",
            json!({ "company_id": f.c.id, "credit_item_id": items[0]["id"], "debt_item_id": items[1]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let items = f.items(&t, "payable").await;
    assert_eq!(items.len(), 1);
    assert_eq!(dec(&items[0]["open"]), -26.2);
    assert_eq!(f.balance(&t, "2000").await, 26.2);
    // The rest is applied, then the payment on account cannot be reversed
    // while it is.
    let (status, body) = run(&t, "payment.reverse", &payment).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    done(&t, "payment.reverse", &applied).await;
    done(&t, "payment.reverse", &payment).await;
    assert_eq!(f.balance(&t, "2000").await, -23.8);
    t.stop().await;
}

impl Fixture {
    async fn deliver(&self, t: &TestApp, order: &Value, qty: &str) -> Value {
        self.posted(
            t,
            "sales_delivery",
            MARCH,
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": Self::line(order), "qty": qty }] }),
        )
        .await
    }

    async fn sales_invoice(&self, t: &TestApp, order: &Value, qty: &str, price: &str) -> Value {
        self.posted(
            t,
            "sales_invoice",
            MARCH,
            json!({
                "sales_order_id": order["id"],
                "lines": [{ "order_line_id": Self::line(order), "qty": qty, "unit_price": price, "tax_code_id": self.vat }],
            }),
        )
        .await
    }

    /// Ten widgets in stock at 2.00 each.
    async fn stock_up(&self, t: &TestApp) {
        let order = self.order(t, "purchase", "10", "2.00").await;
        self.receive(t, &order, "10").await;
    }
}

#[tokio::test]
async fn goods_come_back_from_the_customer_and_are_credited() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock_up(&t).await;

    // Ten sold at 9.00, delivered and invoiced with 19% tax.
    let order = f.order(&t, "sales", "10", "9.00").await;
    f.deliver(&t, &order, "10").await;
    let invoice = f.sales_invoice(&t, &order, "10", "9.00").await;
    assert_eq!(f.balance(&t, "5000").await, 20.0);
    assert_eq!(f.balance(&t, "4000").await, -90.0);
    assert_eq!(f.balance(&t, "1200").await, 107.1);
    assert_eq!(f.now(&t, "sales_order", &order).await["status"], "closed");

    // Four come back in April, at the 2.00 each they left for: cost of
    // goods sold is taken back exactly.
    let (status, draft) = run(&t, "sales_order.return", &order).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["qty"]), 10.0);
    let (status, body) = f
        .try_post(&t, "sales_return", APRIL, f.return_body(&order, "11"))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 10 have been delivered and not returned on order line 1, not 11"
    );
    let ret = f
        .posted(&t, "sales_return", APRIL, f.return_body(&order, "4"))
        .await;
    assert_eq!(dec(&ret["summary"]["total_cost"]), 8.0);
    assert_eq!(f.on_hand(&t).await, (4.0, 8.0));
    assert_eq!(f.balance(&t, "5000").await, 12.0);
    assert_eq!(f.now(&t, "sales_order", &order).await["status"], "closed");

    // The customer is credited for the four, with the tax charged on them.
    let (status, credit) = t
        .command(
            &t.admin,
            "sales_invoice.credit",
            json!({ "id": invoice["id"], "doc_date": APRIL }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    assert_eq!(dec(&credit["body"]["lines"][0]["qty"]), 4.0);
    assert_eq!(dec(&credit["summary"]["total_gross"]), 42.84);
    let credit = done(&t, "sales_credit_note.post", &credit).await;
    assert_eq!(dec(&credit["body"]["lines"][0]["tax_amount"]), 6.84);
    assert_eq!(f.balance(&t, "4000").await, -54.0);
    assert_eq!(f.balance(&t, "2300").await, -10.26);
    assert_eq!(f.balance(&t, "1200").await, 64.26);
    let march = f.report(&t, "2026-03-01", "2026-03-31").await;
    assert_eq!(dec(&march["total_collected"]), 17.1);
    let april = f.report(&t, "2026-04-01", "2026-04-30").await;
    assert_eq!(dec(&april["total_collected"]), -6.84);
    assert_eq!(dec(&april["rows"][0]["collected_base"]), -36.0);

    // A credit beyond what came back is refused by default; undoing what
    // was credited underneath the credit too.
    let invoice_line = &invoice["body"]["lines"][0]["id"];
    let (status, body) = f
        .try_post(
            &t,
            "sales_credit_note",
            APRIL,
            json!({ "sales_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        message(&body).contains("a credit without a return is not accepted"),
        "{body}"
    );
    let (status, body) = run(&t, "sales_return.reverse", &ret).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = run(&t, "sales_invoice.reverse", &invoice).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // One incoming payment settles the invoice less the credit.
    let ids: Vec<Value> = f
        .items(&t, "receivable")
        .await
        .iter()
        .map(|i| i["id"].clone())
        .collect();
    assert_eq!(ids.len(), 2);
    let (status, payment) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": f.c.id, "open_item_ids": ids, "bank_account_id": f.c.cash, "doc_date": APRIL }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{payment}");
    let payment = done(&t, "payment.post", &payment).await;
    assert_eq!(dec(&payment["summary"]["total"]), 64.26);
    assert_eq!(f.balance(&t, "1200").await, 0.0);
    assert!(f.items(&t, "receivable").await.is_empty());

    // Each step can be taken back, last first.
    done(&t, "payment.reverse", &payment).await;
    done(&t, "sales_credit_note.reverse", &credit).await;
    assert_eq!(f.balance(&t, "1200").await, 107.1);
    assert_eq!(f.balance(&t, "2300").await, -17.1);
    done(&t, "sales_return.reverse", &ret).await;
    assert_eq!(f.on_hand(&t).await, (0.0, 0.0));
    assert_eq!(f.balance(&t, "5000").await, 20.0);
    t.stop().await;
}

#[tokio::test]
async fn a_sales_return_before_the_invoice_and_a_credit_for_price_only() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock_up(&t).await;
    let order = f.order(&t, "sales", "10", "9.00").await;
    f.deliver(&t, &order, "10").await;
    // Two come back before invoicing: only eight are invoiced.
    f.posted(&t, "sales_return", MARCH, f.return_body(&order, "2"))
        .await;
    let (status, draft) = run(&t, "sales_order.invoice", &order).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["qty"]), 8.0);
    let invoice = f.sales_invoice(&t, &order, "8", "9.00").await;
    assert_eq!(f.balance(&t, "5000").await, 16.0);
    assert_eq!(f.balance(&t, "1200").await, 85.68);
    assert_eq!(f.now(&t, "sales_order", &order).await["status"], "open");

    // A company that grants allowances credits 1.00 each on all eight,
    // with nothing coming back: revenue and tax only.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "sales_credit_note.without_return", "effect": "allow",
                "condition": "facts.credit_price <= facts.invoice_price * 0.2" }),
    )
    .await;
    let invoice_line = &invoice["body"]["lines"][0]["id"];
    let (status, body) = f
        .try_post(
            &t,
            "sales_credit_note",
            MARCH,
            json!({ "sales_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "8", "unit_price": "5.00" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let credit = f
        .posted(
            &t,
            "sales_credit_note",
            MARCH,
            json!({ "sales_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": invoice_line, "qty": "8", "unit_price": "1.00" }] }),
        )
        .await;
    assert_eq!(dec(&credit["summary"]["total_gross"]), 9.52);
    assert_eq!(f.balance(&t, "4000").await, -64.0);
    assert_eq!(f.balance(&t, "1200").await, 76.16);
    assert_eq!(f.balance(&t, "5000").await, 16.0);
    // Applied to the invoice, the credit leaves 76.16 to pay.
    let items = f.items(&t, "receivable").await;
    let (status, applied) = t
        .command(
            &t.admin,
            "payment.apply_credit",
            json!({ "company_id": f.c.id, "credit_item_id": items[1]["id"], "debt_item_id": items[0]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let items = f.items(&t, "receivable").await;
    assert_eq!(items.len(), 1);
    assert_eq!(dec(&items[0]["open"]), 76.16);
    assert_eq!(f.balance(&t, "1200").await, 76.16);
    t.stop().await;
}

#[tokio::test]
async fn a_command_can_return_only_the_header_of_a_document() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "10", "2.00").await;
    let (status, draft) = f
        .draft(
            &t,
            "purchase_receipt",
            MARCH,
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": Fixture::line(&order), "qty": "10" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");

    // Over HTTP: header and summary, nothing else.
    let (status, posted) = t
        .post(
            &t.admin,
            "/api/commands/purchase_receipt.post?view=header",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    assert_eq!(posted["status"], "posted");
    assert_eq!(dec(&posted["summary"]["total_value"]), 20.0);
    for left_out in ["body", "postings", "links"] {
        assert!(posted.get(left_out).is_none(), "{left_out}: {posted}");
    }
    // The full view is the default, and shows what the posting wrote.
    let full = f.now(&t, "purchase_receipt", &posted).await;
    let entries = &full["postings"][0]["entries"];
    assert!(
        entries["gl"].is_array() && entries["stock"].is_array(),
        "{full}"
    );
    assert!(entries.get("open_items").is_none() && entries.get("tax").is_none());

    // Over MCP: a `view` argument, and a result that still fits the
    // output schema.
    let (status, invoice) = f
        .draft(
            &t,
            "vendor_invoice",
            MARCH,
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": Fixture::line(&order), "qty": "10", "unit_price": "2.00" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    let result = t
        .tool(
            &t.admin,
            "vendor_invoice_post",
            json!({ "id": invoice["id"], "view": "header" }),
        )
        .await;
    let content = &result["structuredContent"];
    assert_eq!(content["status"], "posted", "{result}");
    assert_eq!(content["body"], Value::Null);
    assert_eq!(content["postings"], json!([]));
    // The tool says so, on whichever page of the list it is.
    let mut cursor = Value::Null;
    let post = loop {
        let page = t
            .mcp(&t.admin, "tools/list", json!({ "cursor": cursor }))
            .await;
        if let Some(tool) = page["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "vendor_invoice_post")
        {
            break tool.clone();
        }
        cursor = page["nextCursor"].clone();
        assert!(!cursor.is_null(), "vendor_invoice_post is listed");
    };
    assert_eq!(
        post["inputSchema"]["properties"]["view"]["enum"],
        json!(["full", "header"])
    );
    let (status, body) = t
        .post(
            &t.admin,
            "/api/commands/vendor_invoice.post?view=everything",
            json!({ "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    t.stop().await;
}

// ------------------------------------------------- fixes from the review

#[tokio::test]
async fn a_payment_moves_money_only_through_a_bank_account_and_keeps_credits_on_control_accounts() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "10", "2.00").await;
    f.receive(&t, &order, "10").await;
    f.vendor_invoice(&t, &order, "10", "2.00").await;
    let item = f.items(&t, "payable").await[0]["id"].clone();

    // Through the receivables account: refused, as through any account
    // that is not a bank or cash account.
    for bank in [&f.account["1200"], &f.account["5200"]] {
        let (status, body) = f
            .draft(
                &t,
                "payment",
                MARCH,
                json!({ "direction": "outgoing", "bank_account_id": bank,
                        "allocations": [{ "open_item_id": item, "amount": "1.00" }] }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["field"], "bank_account_id", "{body}");
    }
    // Money on account is kept on the payables account, for a vendor.
    for (account, party_type, party, field) in [
        (&f.account["5200"], "vendor", &f.vendor, "account_id"),
        (&f.account["1200"], "vendor", &f.vendor, "account_id"),
        (&f.account["2000"], "customer", &f.customer, "party_type"),
    ] {
        let (status, body) = f
            .draft(
                &t,
                "payment",
                MARCH,
                json!({ "direction": "outgoing", "bank_account_id": f.c.cash, "on_account": "5.00",
                        "party_type": party_type, "party_id": party, "account_id": account }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
    }
    f.balance(&t, "2000").await;
    t.stop().await;
}

#[tokio::test]
async fn a_credit_note_takes_back_no_more_than_the_invoice_line_charged() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Credits without a return are allowed, so only the cap stands.
    for point in [
        "vendor_credit_note.without_return",
        "sales_credit_note.without_return",
    ] {
        t.create(
            "policy_rule",
            json!({ "company_id": f.c.id, "point": point, "effect": "allow", "condition": null }),
        )
        .await;
    }
    let order = f.order(&t, "purchase", "10", "2.00").await;
    f.receive(&t, &order, "10").await;
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            MARCH,
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": Fixture::line(&order), "qty": "10", "unit_price": "2.00" }] }),
        )
        .await;
    let invoice_line = invoice["body"]["lines"][0]["id"].clone();
    let credit = |qty: &'static str, price: &'static str| {
        json!({ "vendor_invoice_id": invoice["id"],
                "lines": [{ "invoice_line_id": invoice_line, "qty": qty, "unit_price": price }] })
    };
    let (status, body) = f
        .try_post(&t, "vendor_credit_note", MARCH, credit("1", "1000000"))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 20 of what the invoice line charged is left to credit, not 1000000"
    );
    assert_eq!(body["error"]["field"], "unit_price");
    f.posted(&t, "vendor_credit_note", MARCH, credit("4", "3.00"))
        .await;
    let (status, body) = f
        .try_post(&t, "vendor_credit_note", MARCH, credit("4", "2.10"))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).starts_with("line 1: 8 of what"), "{body}");
    // Past the handler, the database holds to the same.
    let raw = sqlx::query(
        "UPDATE vendor_credit_note_line SET net_amount = 1000 WHERE net_amount IS NOT NULL",
    )
    .execute(&t.pool)
    .await;
    assert!(raw.is_err());
    f.balance(&t, "2000").await;

    // Sales alike.
    f.stock_up(&t).await;
    let sale = f.order(&t, "sales", "2", "9.00").await;
    f.deliver(&t, &sale, "2").await;
    let sold = f
        .posted(
            &t,
            "sales_invoice",
            MARCH,
            json!({ "sales_order_id": sale["id"],
                    "lines": [{ "order_line_id": Fixture::line(&sale), "qty": "2", "unit_price": "9.00" }] }),
        )
        .await;
    let (status, body) = f
        .try_post(
            &t,
            "sales_credit_note",
            MARCH,
            json!({ "sales_invoice_id": sold["id"],
                    "lines": [{ "invoice_line_id": sold["body"]["lines"][0]["id"], "qty": "1", "unit_price": "1000000" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "line 1: 18 of what the invoice line charged is left to credit, not 1000000"
    );
    t.stop().await;
}

#[tokio::test]
async fn a_delivery_is_not_taken_back_from_under_a_return_at_the_average_cost() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Two widgets bought at 5.00 and delivered (10.00), then two at 7.00
    // and delivered (14.00), on one order line.
    let sale = f.order(&t, "sales", "4", "9.00").await;
    let mut deliveries = vec![];
    for price in ["5.00", "7.00"] {
        let order = f.order(&t, "purchase", "2", price).await;
        f.receive(&t, &order, "2").await;
        deliveries.push(f.deliver(&t, &sale, "2").await);
    }
    let (first, second) = (deliveries[0].clone(), deliveries[1].clone());
    // Two come back at the average cost of the deliveries.
    let ret = f
        .posted(&t, "sales_return", MARCH, f.return_body(&sale, "2"))
        .await;
    assert_eq!(dec(&ret["summary"]["total_cost"]), 12.0);
    for delivery in [&second, &first] {
        let (status, body) = run(&t, "sales_delivery.reverse", delivery).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            message(&body).contains("reverse that return first"),
            "{body}"
        );
    }
    // Once the return is taken back, so can the deliveries be.
    done(&t, "sales_return.reverse", &ret).await;
    done(&t, "sales_delivery.reverse", &second).await;
    done(&t, "sales_delivery.reverse", &first).await;
    assert_eq!(f.on_hand(&t).await, (4.0, 24.0));
    assert_eq!(f.balance(&t, "5000").await, 0.0);
    t.stop().await;
}

#[tokio::test]
async fn a_credit_takes_tax_back_from_the_account_the_invoice_posted_it_to() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "10", "2.00").await;
    f.receive(&t, &order, "10").await;
    let invoice = f.vendor_invoice(&t, &order, "10", "2.00").await;
    assert_eq!(f.balance(&t, "1400").await, 3.8);
    // The company names another account for the tax it pays.
    let other = t
        .create(
            "account",
            json!({ "company_id": f.c.id, "code": "1450", "name": "1450", "account_type": "asset" }),
        )
        .await;
    let (_, settings) = t
        .get(
            &t.admin,
            &format!("/api/entities/tax_settings?company_id={}", f.c.id),
        )
        .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/tax_settings/{}", id(&settings["items"][0])),
            json!({ "paid_account_id": other }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    f.posted(&t, "purchase_return", MARCH, f.return_body(&order, "10"))
        .await;
    let (status, credit) = t
        .command(
            &t.admin,
            "vendor_invoice.credit",
            json!({ "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    done(&t, "vendor_credit_note.post", &credit).await;
    // Taken back from 1400, where the invoice put it; 1450 is untouched.
    assert_eq!(f.balance(&t, "1400").await, 0.0);
    let (_, tb) = t
        .command(
            &t.admin,
            "gl.trial_balance",
            json!({ "company_id": f.c.id }),
        )
        .await;
    assert!(
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["code"] != "1450" || dec(&r["balance"]) == 0.0)
    );
    t.stop().await;
}

#[tokio::test]
async fn reversals_out_of_order_are_refused_in_words_when_they_would_unbalance_a_line() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Three received for 10.00 and invoiced.
    let order = f.order(&t, "purchase", "3", "3.333333").await;
    f.receive(&t, &order, "3").await;
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            MARCH,
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": Fixture::line(&order), "qty": "3", "unit_price": "3.333333" }] }),
        )
        .await;
    let invoice_line = invoice["body"]["lines"][0]["id"].clone();
    let credit_one = || {
        json!({ "vendor_invoice_id": invoice["id"],
                "lines": [{ "invoice_line_id": invoice_line, "qty": "1" }] })
    };
    // One back and credited (3.33), then another (3.34).
    f.posted(&t, "purchase_return", MARCH, f.return_body(&order, "1"))
        .await;
    let first_credit = f
        .posted(&t, "vendor_credit_note", MARCH, credit_one())
        .await;
    let second_return = f
        .posted(&t, "purchase_return", MARCH, f.return_body(&order, "1"))
        .await;
    f.posted(&t, "vendor_credit_note", MARCH, credit_one())
        .await;
    done(&t, "vendor_credit_note.reverse", &first_credit).await;
    // Taking back the second return now would leave a cent in GRNI on a
    // line that is all invoiced: refused, in words, not by the database.
    let (status, body) = run(&t, "purchase_return.reverse", &second_return).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        message(&body).contains("take back the returns and credit notes posted after it"),
        "{body}"
    );
    f.balance(&t, "2100").await;
    t.stop().await;
}
