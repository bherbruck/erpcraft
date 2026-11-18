//! Buying and selling in foreign currencies: a US company buys in euros
//! and Canadian dollars and sells in Canadian dollars, with rates changing
//! between receipt, invoice and payment. The books are checked at every
//! step: the trial balance balances, the inventory account equals the
//! value of the stock, open items equal the payables and receivables
//! accounts in the accounting currency, the tax report equals the tax
//! accounts, and a settled item leaves nothing behind.

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
        .create("vendor", json!({ "code": "V-EU", "name": "Teile GmbH" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C-CA", "name": "Maple Ltd" }))
        .await;
    let mut account = BTreeMap::new();
    for (code, kind, currency) in [
        ("1010", "asset", Some("EUR")),
        ("1020", "asset", Some("CAD")),
        ("1200", "asset", None),
        ("1300", "asset", None),
        ("1400", "asset", None),
        ("2000", "liability", None),
        ("2100", "liability", None),
        ("2300", "liability", None),
        ("5000", "expense", None),
        ("5200", "expense", None),
        ("7100", "income", None),
        ("7200", "expense", None),
    ] {
        let mut data =
            json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind });
        if let Some(currency) = currency {
            data["currency"] = json!(currency);
            data["is_bank"] = json!(true);
        }
        account.insert(code, t.create("account", data).await);
    }
    t.create(
        "purchasing_settings",
        json!({ "company_id": c.id, "grni_account_id": account["2100"],
                "payable_account_id": account["2000"], "price_variance_account_id": account["5200"] }),
    )
    .await;
    t.create(
        "sales_settings",
        json!({ "company_id": c.id, "receivable_account_id": account["1200"],
                "revenue_account_id": c.revenue, "cogs_account_id": account["5000"] }),
    )
    .await;
    t.inventory_rule(&c.id, &account["1300"]).await;
    for (purpose, code) in [("fx_gain", "7100"), ("fx_loss", "7200")] {
        t.create(
            "account_rule",
            json!({ "company_id": c.id, "purpose": purpose, "account_id": account[code] }),
        )
        .await;
    }
    let vat = t
        .create(
            "tax_code",
            json!({ "code": "VAT19", "name": "VAT 19%", "rate": "19" }),
        )
        .await;
    t.create(
        "tax_settings",
        json!({ "company_id": c.id, "tax_code_id": vat,
                "collected_account_id": account["2300"], "paid_account_id": account["1400"] }),
    )
    .await;
    // Dollars per unit, from the dates given.
    for (currency, date, rate) in [
        ("EUR", "2026-03-01", "1.10"),
        ("EUR", "2026-03-20", "1.20"),
        ("EUR", "2026-04-01", "1.05"),
        ("CAD", "2026-03-01", "0.75"),
        ("CAD", "2026-04-01", "0.70"),
    ] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": currency, "to_currency": "USD", "rate_date": date, "rate": rate }),
        )
        .await;
    }
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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

impl Fixture {
    async fn draft(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
        draft
    }

    async fn try_post(
        &self,
        t: &TestApp,
        doc_type: &str,
        date: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let draft = self.draft(t, doc_type, date, body).await;
        t.command(
            &t.admin,
            &format!("{doc_type}.post"),
            json!({ "id": id(&draft) }),
        )
        .await
    }

    async fn posted(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, done) = self.try_post(t, doc_type, date, body).await;
        assert_eq!(status, StatusCode::OK, "{doc_type}: {done}");
        done
    }

    async fn order(
        &self,
        t: &TestApp,
        kind: &str,
        currency: &str,
        qty: &str,
        price: &str,
    ) -> Value {
        let mut body = match kind {
            "purchase" => json!({ "vendor_id": self.vendor }),
            _ => json!({ "customer_id": self.customer }),
        };
        body["currency"] = json!(currency);
        body["lines"] = json!([{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }]);
        let draft = self
            .draft(t, &format!("{kind}_order"), "2026-03-02", body)
            .await;
        let (status, order) = t
            .command(
                &t.admin,
                &format!("{kind}_order.approve"),
                json!({ "id": id(&draft) }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{order}");
        order
    }

    /// Balances by account code; on the way the books are checked.
    async fn books(&self, t: &TestApp) -> BTreeMap<String, f64> {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        let balances: BTreeMap<String, f64> = tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["code"].as_str().unwrap().to_owned(), dec(&r["balance"])))
            .collect();
        let at = |code: &str| balances.get(code).copied().unwrap_or(0.0);
        let (_, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        assert!(
            close(dec(&on_hand["total_value"]), at("1300")),
            "stock value and the inventory account agree"
        );
        for (kind, account, sign) in [("payable", "2000", -1.0), ("receivable", "1200", 1.0)] {
            let (_, items) = t
                .command(
                    &t.admin,
                    "open_item.list",
                    json!({ "company_id": self.c.id, "kind": kind }),
                )
                .await;
            assert!(
                close(dec(&items["total_open"]), sign * at(account)),
                "open {kind}s {} equal account {account} {}",
                items["total_open"],
                at(account)
            );
        }
        let (_, report) = t
            .command(
                &t.admin,
                "tax.report",
                json!({ "company_id": self.c.id, "from": "2026-01-01", "to": "2026-12-31" }),
            )
            .await;
        assert!(close(dec(&report["total_paid"]), at("1400")), "{report}");
        assert!(
            close(dec(&report["total_collected"]), -at("2300")),
            "{report}"
        );
        // Nothing is left on a settled item.
        assert_eq!(
            t.count(
                "SELECT count(*) FROM open_item WHERE txn_settled = txn_amount AND settled <> amount"
            )
            .await,
            0
        );
        balances
    }

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
}

#[tokio::test]
async fn bought_in_euros_received_invoiced_credited_and_paid_at_other_rates() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Ten at 2.00 EUR. Received on March 15 at 1.10: 22.00 USD in stock and
    // in GRNI.
    let order = f.order(&t, "purchase", "EUR", "10", "2.00").await;
    assert_eq!(order["summary"]["currency"], "EUR");
    assert_eq!(dec(&order["summary"]["total_net"]), 20.0);
    let line = order["body"]["lines"][0]["id"].clone();
    let receipt = f
        .posted(
            &t,
            "purchase_receipt",
            "2026-03-15",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
        )
        .await;
    assert_eq!(dec(&receipt["body"]["fx_rate"]), 1.1);
    let books = f.books(&t).await;
    assert!(close(books["1300"], 22.0));
    assert!(close(books["2100"], -22.0));

    // Invoiced on March 25 at 1.20, with 19 % tax: 23.80 EUR owed, 28.56
    // USD. GRNI clears; the 2.00 USD the goods now cost more is price
    // variance.
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-25",
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": "10", "unit_price": "2.00", "tax_code_id": f.vat }] }),
        )
        .await;
    assert_eq!(invoice["summary"]["currency"], "EUR");
    assert_eq!(dec(&invoice["summary"]["total_gross"]), 23.8);
    // In the accounting currency, what was posted, to the cent.
    assert_eq!(dec(&invoice["summary"]["total_gross_accounting"]), 28.56);
    assert_eq!(dec(&invoice["summary"]["total_tax_accounting"]), 4.56);
    assert_eq!(dec(&invoice["summary"]["total_net_accounting"]), 24.0);
    let books = f.books(&t).await;
    assert!(close(books["2100"], 0.0));
    assert!(close(books["2000"], -28.56));
    assert!(close(books["5200"], 2.0));
    assert!(close(books["1400"], 4.56));
    let items = f.items(&t, "payable").await;
    assert_eq!(items[0]["currency"], "EUR");
    assert_eq!(dec(&items[0]["txn_open"]), 23.8);

    // Two go back; the vendor credits them at the invoice's rate, tax and
    // all: 4.76 EUR, 5.71 USD.
    f.posted(
        &t,
        "purchase_return",
        "2026-03-26",
        json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "2" }] }),
    )
    .await;
    let (status, credit) = t
        .command(
            &t.admin,
            "vendor_invoice.credit",
            json!({ "id": invoice["id"], "doc_date": "2026-03-27" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    assert!(
        credit["summary"]["total_gross_accounting"].is_null(),
        "a draft in another currency has no accounting totals yet"
    );
    let credit = run_post(&t, "vendor_credit_note.post", &credit).await;
    assert_eq!(credit["body"]["currency"], "EUR");
    assert_eq!(dec(&credit["summary"]["total_gross"]), 4.76);
    assert_eq!(dec(&credit["summary"]["total_gross_accounting"]), 5.71);
    assert_eq!(dec(&credit["summary"]["total_tax_accounting"]), 0.91);
    assert_eq!(dec(&credit["summary"]["total_net_accounting"]), 4.8);
    let books = f.books(&t).await;
    assert!(close(books["2100"], 0.0), "GRNI clears");
    assert!(close(books["2000"], -(28.56 - 5.71)));
    assert!(close(books["1400"], 4.56 - 0.91));

    // A payment in euros on April 2, at 1.05, nets the invoice and the
    // credit: 19.04 EUR, 19.99 USD for items booked at 22.85 USD. The 2.86
    // is a realized gain.
    let ids: Vec<Value> = f
        .items(&t, "payable")
        .await
        .iter()
        .map(|i| i["id"].clone())
        .collect();
    let (status, payment) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": f.c.id, "open_item_ids": ids,
                    "bank_account_id": f.account["1010"], "doc_date": "2026-04-02" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{payment}");
    assert_eq!(payment["body"]["currency"], "EUR");
    assert_eq!(dec(&payment["body"]["amount"]), 19.04);
    assert_eq!(payment["summary"]["currency"], "EUR");
    let payment = run_post(&t, "payment.post", &payment).await;
    assert_eq!(dec(&payment["body"]["fx_rate"]), 1.05);
    let books = f.books(&t).await;
    assert!(close(books["2000"], 0.0), "payables clear to the cent");
    assert!(close(books["1010"], -19.99));
    assert!(close(books["7100"], -2.86), "realized gain");
    assert!(f.items(&t, "payable").await.is_empty());

    // Taking the payment back takes the gain back with it.
    run_post(&t, "payment.reverse", &payment).await;
    let books = f.books(&t).await;
    assert!(close(books["7100"], 0.0));
    assert!(close(books["2000"], -(28.56 - 5.71)));
    t.stop().await;
}

#[tokio::test]
async fn sold_in_canadian_dollars_and_paid_in_parts() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Stock bought in dollars.
    let buy = f.order(&t, "purchase", "USD", "10", "2.00").await;
    f.posted(
        &t,
        "purchase_receipt",
        "2026-03-03",
        json!({ "purchase_order_id": buy["id"], "lines": [{ "order_line_id": buy["body"]["lines"][0]["id"], "qty": "10" }] }),
    )
    .await;

    // Four sold at 9.00 CAD, invoiced on March 25 at 0.75: 36.00 CAD of
    // revenue is 27.00 USD, the tax 6.84 CAD is 5.13 USD. Cost of goods
    // sold stays 8.00 USD, from the stock ledger.
    let order = f.order(&t, "sales", "CAD", "4", "9.00").await;
    let line = order["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-20",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "4" }] }),
    )
    .await;
    let invoice = f
        .posted(
            &t,
            "sales_invoice",
            "2026-03-25",
            json!({ "sales_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": "4", "unit_price": "9.00", "tax_code_id": f.vat }] }),
        )
        .await;
    assert_eq!(dec(&invoice["summary"]["total_gross"]), 42.84);
    assert_eq!(dec(&invoice["summary"]["total_gross_accounting"]), 32.13);
    assert_eq!(dec(&invoice["summary"]["total_tax_accounting"]), 5.13);
    assert_eq!(dec(&invoice["summary"]["total_net_accounting"]), 27.0);
    let books = f.books(&t).await;
    assert!(close(books["4000"], -27.0));
    assert!(close(books["2300"], -5.13));
    assert!(close(books["5000"], 8.0));
    assert!(close(books["1200"], 32.13));

    // Paid in two parts on April 2 at 0.70. 20.00 CAD settles 15.00 USD of
    // the item and brings 14.00 USD: a loss of 1.00.
    let item = f.items(&t, "receivable").await[0]["id"].clone();
    let pay = |amount: &'static str| {
        let t = &t;
        let f = &f;
        let item = item.clone();
        async move {
            f.posted(
                t,
                "payment",
                "2026-04-02",
                json!({ "direction": "incoming", "bank_account_id": f.account["1020"],
                        "allocations": [{ "open_item_id": item, "amount": amount }] }),
            )
            .await
        }
    };
    pay("20.00").await;
    let books = f.books(&t).await;
    assert!(close(books["1200"], 17.13));
    assert!(close(books["1020"], 14.0));
    assert!(close(books["7200"], 1.0), "realized loss");
    // The rest, 22.84 CAD, takes the rest of the item's value, 17.13 USD,
    // for 15.99 USD: nothing is left of it.
    pay("22.84").await;
    let books = f.books(&t).await;
    assert!(close(books["1200"], 0.0));
    assert!(close(books["7200"], 2.14));
    assert!(f.items(&t, "receivable").await.is_empty());
    t.stop().await;
}

#[tokio::test]
async fn currencies_are_not_mixed_and_a_stated_rate_can_be_checked() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // One invoice in euros, one in Canadian dollars, from one vendor.
    let mut invoices = vec![];
    for currency in ["EUR", "CAD"] {
        let order = f.order(&t, "purchase", currency, "1", "10.00").await;
        let line = order["body"]["lines"][0]["id"].clone();
        f.posted(
            &t,
            "purchase_receipt",
            "2026-03-15",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "1" }] }),
        )
        .await;
        invoices.push(
            f.posted(
                &t,
                "vendor_invoice",
                "2026-03-25",
                json!({ "purchase_order_id": order["id"],
                        "lines": [{ "order_line_id": line, "qty": "1", "unit_price": "10.00" }] }),
            )
            .await,
        );
    }
    let ids: Vec<Value> = f
        .items(&t, "payable")
        .await
        .iter()
        .map(|i| i["id"].clone())
        .collect();
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({ "company_id": f.c.id, "doc_date": "2026-04-02",
                    "body": { "direction": "outgoing",
                              "allocations": [{ "open_item_id": ids[0], "amount": "1" }, { "open_item_id": ids[1], "amount": "1" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("one currency"), "{body}");
    // The euro bank account takes only euros.
    let cad = f
        .items(&t, "payable")
        .await
        .into_iter()
        .find(|i| i["currency"] == "CAD")
        .unwrap();
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({ "company_id": f.c.id, "doc_date": "2026-04-02",
                    "body": { "direction": "outgoing", "bank_account_id": f.account["1010"],
                              "allocations": [{ "open_item_id": cad["id"], "amount": "1" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("holds EUR"), "{body}");

    // A stated rate far from the rate on record is refused where the
    // company says so.
    t.create(
        "policy_rule",
        json!({ "company_id": f.c.id, "point": "fx.rate_differs", "effect": "refuse",
                "condition": "facts.difference_percent > 2 || facts.difference_percent < -2",
                "message": "{stated} is {difference_percent}% off the rate on record {on_record}" }),
    )
    .await;
    let order = f.order(&t, "purchase", "EUR", "1", "10.00").await;
    let line = order["body"]["lines"][0]["id"].clone();
    let (status, body) = f
        .try_post(
            &t,
            "purchase_receipt",
            "2026-03-15",
            json!({ "purchase_order_id": order["id"], "fx_rate": "1.5",
                    "lines": [{ "order_line_id": line, "qty": "1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        message(&body),
        "1.5 is 36.363636% off the rate on record 1.1"
    );
    let receipt = f
        .posted(
            &t,
            "purchase_receipt",
            "2026-03-15",
            json!({ "purchase_order_id": order["id"], "fx_rate": "1.11",
                    "lines": [{ "order_line_id": line, "qty": "1" }] }),
        )
        .await;
    assert_eq!(dec(&receipt["body"]["lines"][0]["value"]), 11.1);
    f.books(&t).await;
    let _ = invoices;
    t.stop().await;
}

async fn run_post(t: &TestApp, command: &str, doc: &Value) -> Value {
    let (status, body) = t.command(&t.admin, command, json!({ "id": id(doc) })).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

#[tokio::test]
async fn money_on_account_in_a_currency_is_applied_at_its_own_rate() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let buy = f.order(&t, "purchase", "USD", "10", "2.00").await;
    f.posted(
        &t,
        "purchase_receipt",
        "2026-03-03",
        json!({ "purchase_order_id": buy["id"], "lines": [{ "order_line_id": buy["body"]["lines"][0]["id"], "qty": "10" }] }),
    )
    .await;
    let order = f.order(&t, "sales", "CAD", "4", "9.00").await;
    let line = order["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-20",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "4" }] }),
    )
    .await;
    f.posted(
        &t,
        "sales_invoice",
        "2026-03-25",
        json!({ "sales_order_id": order["id"],
                "lines": [{ "order_line_id": line, "qty": "4", "unit_price": "9.00" }] }),
    )
    .await;
    // 10.00 CAD paid on account on April 2 at 0.70: 7.00 USD.
    let paid = f
        .posted(
            &t,
            "payment",
            "2026-04-02",
            json!({ "direction": "incoming", "bank_account_id": f.account["1020"],
                    "on_account": "10.00", "party_type": "customer", "party_id": f.customer,
                    "account_id": f.account["1200"] }),
        )
        .await;
    assert_eq!(paid["body"]["currency"], "CAD");
    let books = f.books(&t).await;
    assert!(close(books["1200"], 27.0 - 7.0));
    // Applied to the invoice booked at 0.75, the 10.00 CAD take 7.50 USD
    // off it: 0.50 less came in than the invoice was booked at.
    let items = f.items(&t, "receivable").await;
    let credit = items.iter().find(|i| dec(&i["amount"]) < 0.0).unwrap();
    let debt = items.iter().find(|i| dec(&i["amount"]) > 0.0).unwrap();
    let (status, applied) = t
        .command(
            &t.admin,
            "payment.apply_credit",
            json!({ "company_id": f.c.id, "credit_item_id": credit["id"], "debt_item_id": debt["id"],
                    "doc_date": "2026-04-03" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    let books = f.books(&t).await;
    assert!(close(books["7200"], 0.5), "realized when applied");
    assert!(close(books["1200"], 27.0 - 7.5));
    let items = f.items(&t, "receivable").await;
    assert_eq!(items.len(), 1);
    assert_eq!(dec(&items[0]["txn_open"]), 26.0);
    t.stop().await;
}

#[tokio::test]
async fn a_company_set_up_from_a_template_knows_where_exchange_differences_go() {
    let t = TestApp::start().await;
    let (status, created) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_small_business", "code": "US1", "name": "US One", "first_year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    for purpose in ["fx_gain", "fx_loss"] {
        assert_eq!(
            t.count(&format!(
                "SELECT count(*) FROM account_rule WHERE purpose = '{purpose}'"
            ))
            .await,
            1,
            "{purpose}"
        );
    }
    t.stop().await;
}

/// Every field of `kind` link in the metadata, with where it is: the
/// header, a collection or the summary of a document type, or an entity.
fn links(meta: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let mut walk = |place: String, fields: &Value| {
        for field in fields.as_array().into_iter().flatten() {
            if field["type"]["kind"] == "link" {
                found.push((
                    format!("{place}.{}", field["name"].as_str().unwrap()),
                    field.clone(),
                ));
            }
        }
    };
    for entity in meta["entities"].as_array().unwrap() {
        walk(
            entity["name"].as_str().unwrap().to_owned(),
            &entity["fields"],
        );
    }
    for doc_type in meta["doc_types"].as_array().unwrap() {
        let name = doc_type["name"].as_str().unwrap();
        walk(name.to_owned(), &doc_type["fields"]);
        walk(format!("{name}.summary"), &doc_type["summary"]);
        for collection in doc_type["collections"].as_array().into_iter().flatten() {
            walk(
                format!("{name}.{}", collection["name"].as_str().unwrap()),
                &collection["fields"],
            );
        }
    }
    found
}

#[tokio::test]
async fn a_link_says_which_key_its_value_is() {
    let t = TestApp::start().await;
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");

    // Currencies are referred to by their code everywhere.
    let wrong: Vec<String> = links(&meta)
        .into_iter()
        .filter(|(_, f)| f["type"]["entity"] == "currency" && f["type"]["key"] != "code")
        .map(|(place, f)| format!("{place}: key {}", f["type"]["key"]))
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");

    // And what the documents store is that key of a record: every link in
    // the body, lines and summary of documents in another currency finds
    // its record by the key it declares.
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "EUR", "10", "2.00").await;
    let line = order["body"]["lines"][0]["id"].clone();
    let receipt = f
        .posted(
            &t,
            "purchase_receipt",
            "2026-03-15",
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
        )
        .await;
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-25",
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": "10", "unit_price": "2.00", "tax_code_id": f.vat }] }),
        )
        .await;
    let item = f.items(&t, "payable").await[0]["id"].clone();
    let payment = f
        .posted(
            &t,
            "payment",
            "2026-04-02",
            json!({ "direction": "outgoing", "bank_account_id": f.account["1010"],
                    "allocations": [{ "open_item_id": item, "amount": "23.80" }] }),
        )
        .await;
    let doc_types = meta["doc_types"].as_array().unwrap();
    let mut checked = 0;
    for doc in [&order, &receipt, &invoice, &payment] {
        let doc_type = doc_types
            .iter()
            .find(|d| d["name"] == doc["doc_type"])
            .unwrap_or_else(|| panic!("no doc type for {doc}"));
        let mut values: Vec<(&Value, &Value)> = Vec::new();
        let fields = |list: &'static str| doc_type[list].as_array().unwrap().iter();
        for field in fields("fields") {
            values.push((field, &doc["body"][field["name"].as_str().unwrap()]));
        }
        for field in fields("summary") {
            values.push((field, &doc["summary"][field["name"].as_str().unwrap()]));
        }
        for collection in doc_type["collections"].as_array().into_iter().flatten() {
            let name = collection["name"].as_str().unwrap();
            for line in doc["body"][name].as_array().into_iter().flatten() {
                for field in collection["fields"].as_array().unwrap() {
                    values.push((field, &line[field["name"].as_str().unwrap()]));
                }
            }
        }
        for (field, value) in values {
            let (Some(value), "link") =
                (value.as_str(), field["type"]["kind"].as_str().unwrap_or(""))
            else {
                continue;
            };
            let entity = field["type"]["entity"].as_str().unwrap();
            let key = field["type"]["key"].as_str().unwrap();
            let (status, listed) = t
                .command(
                    &t.admin,
                    "entity.list",
                    json!({ "entity": entity, "limit": 500 }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{listed}");
            assert!(
                listed["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r[key].as_str() == Some(value)),
                "{}.{}: {value} is no {entity}'s {key}",
                doc["doc_type"],
                field["name"]
            );
            checked += 1;
        }
    }
    // The currency of each, at least, and the parties and accounts.
    assert!(checked >= 6, "only {checked} links checked");
    t.stop().await;
}
