//! Payment terms, early-payment discounts and aging: a vendor's and a
//! customer's terms set when their invoices are due and the discount for
//! paying early; a discount taken in time is posted with the tax on it
//! taken back; aging puts what is open in buckets that add up to the
//! payables and receivables accounts, now and as of a past day.

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
    net30: String,
    eom30: String,
    two_ten: String,
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
    let net30 = t
        .create(
            "payment_terms",
            json!({ "code": "NET30", "name": "Net 30", "net_days": 30 }),
        )
        .await;
    let eom30 = t
        .create(
            "payment_terms",
            json!({ "code": "EOM30", "name": "30 days end of month", "net_days": 30,
                    "end_of_month": true }),
        )
        .await;
    let two_ten = t
        .create(
            "payment_terms",
            json!({ "code": "2-10-NET30", "name": "2 % 10, net 30", "net_days": 30,
                    "discount_percent": "2", "discount_days": 10 }),
        )
        .await;
    let vendor = t
        .create(
            "vendor",
            json!({ "code": "V1", "name": "Parts Inc", "payment_terms_id": net30 }),
        )
        .await;
    let customer = t
        .create(
            "customer",
            json!({ "code": "C1", "name": "Buyer Ltd", "payment_terms_id": two_ten }),
        )
        .await;
    let mut account = BTreeMap::new();
    for (code, kind, currency) in [
        ("1010", "asset", Some("EUR")),
        ("1200", "asset", None),
        ("1300", "asset", None),
        ("1400", "asset", None),
        ("2000", "liability", None),
        ("2100", "liability", None),
        ("2300", "liability", None),
        ("4030", "income", None),
        ("5000", "expense", None),
        ("5040", "expense", None),
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
    for (purpose, code) in [
        ("fx_gain", "7100"),
        ("fx_loss", "7200"),
        ("discount_received", "5040"),
        ("discount_given", "4030"),
    ] {
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
    for (date, rate) in [("2026-01-01", "1.10"), ("2026-04-01", "1.20")] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": date, "rate": rate }),
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
        net30,
        eom30,
        two_ten,
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

    async fn post(&self, t: &TestApp, doc_type: &str, draft: &Value) -> (StatusCode, Value) {
        t.command(
            &t.admin,
            &format!("{doc_type}.post"),
            json!({ "id": id(draft) }),
        )
        .await
    }

    async fn posted(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let draft = self.draft(t, doc_type, date, body).await;
        let (status, done) = self.post(t, doc_type, &draft).await;
        assert_eq!(status, StatusCode::OK, "{doc_type}: {done}");
        done
    }

    /// An approved order of `qty` at `price`, with `extra` in its body.
    async fn order(&self, t: &TestApp, kind: &str, qty: &str, price: &str, extra: Value) -> Value {
        let mut body = match kind {
            "purchase" => json!({ "vendor_id": self.vendor }),
            _ => json!({ "customer_id": self.customer }),
        };
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body["lines"] = json!([{ "item_id": self.item, "warehouse_id": self.warehouse,
                                  "qty": qty, "unit_price": price }]);
        let draft = self
            .draft(t, &format!("{kind}_order"), "2026-01-02", body)
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

    /// Bought, received and invoiced on `date`, with 19 % tax, with
    /// `extra` in the invoice's body.
    async fn bought(&self, t: &TestApp, order: &Value, date: &str, extra: Value) -> Value {
        let line = order["body"]["lines"][0]["id"].clone();
        let qty = order["body"]["lines"][0]["qty"].clone();
        let price = order["body"]["lines"][0]["unit_price"].clone();
        self.posted(
            t,
            "purchase_receipt",
            date,
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        let mut body = json!({ "purchase_order_id": order["id"],
            "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price, "tax_code_id": self.vat }] });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        self.posted(t, "vendor_invoice", date, body).await
    }

    /// Delivered and invoiced on `date`, with 19 % tax. Stock is bought
    /// first.
    async fn sold(&self, t: &TestApp, order: &Value, date: &str) -> Value {
        let line = order["body"]["lines"][0]["id"].clone();
        let qty = order["body"]["lines"][0]["qty"].clone();
        let price = order["body"]["lines"][0]["unit_price"].clone();
        let stock = self.order(t, "purchase", "100", "1.00", json!({})).await;
        self.posted(
            t,
            "purchase_receipt",
            "2026-01-02",
            json!({ "purchase_order_id": stock["id"],
                    "lines": [{ "order_line_id": stock["body"]["lines"][0]["id"], "qty": "100" }] }),
        )
        .await;
        self.posted(
            t,
            "sales_delivery",
            date,
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        self.posted(
            t,
            "sales_invoice",
            date,
            json!({ "sales_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price, "tax_code_id": self.vat }] }),
        )
        .await
    }

    async fn item_of(&self, t: &TestApp, kind: &str, invoice: &Value) -> Value {
        let (_, items) = t
            .command(
                &t.admin,
                "open_item.list",
                json!({ "company_id": self.c.id, "kind": kind, "include_settled": true, "limit": 500 }),
            )
            .await;
        items["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["document_id"] == invoice["id"])
            .cloned()
            .unwrap_or_else(|| panic!("no item for {invoice}"))
    }

    /// Balances by account code, as of a day; on the way the books are
    /// checked against open items and the tax report.
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
        balances
    }

    async fn aging(&self, t: &TestApp, kind: &str, as_of: &str) -> Value {
        let (status, aging) = t
            .command(
                &t.admin,
                "open_item.aging",
                json!({ "company_id": self.c.id, "kind": kind, "as_of": as_of }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{aging}");
        // The buckets add up, and so do the items and their accounts.
        let totals: f64 = aging["totals"].as_array().unwrap().iter().map(dec).sum();
        assert!(close(totals, dec(&aging["total"])), "{aging}");
        let accounts = aging["accounts"].as_array().unwrap();
        let balance: f64 = accounts.iter().map(|a| dec(&a["balance"])).sum();
        assert!(
            close(balance, dec(&aging["total"])),
            "the {kind} accounts hold what the items add up to as of {as_of}: {aging}"
        );
        for account in accounts {
            assert!(
                close(dec(&account["balance"]), dec(&account["open"])),
                "{aging}"
            );
        }
        aging
    }
}

#[tokio::test]
async fn terms_set_due_dates_from_the_invoice_date_or_the_end_of_its_month() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // The vendor's terms come to its order and from the order to its
    // invoice: net 30 from March 25.
    let order = f.order(&t, "purchase", "10", "10.00", json!({})).await;
    assert_eq!(order["body"]["payment_terms_id"], json!(f.net30));
    let invoice = f.bought(&t, &order, "2026-03-25", json!({})).await;
    assert_eq!(invoice["body"]["payment_terms_id"], json!(f.net30));
    let item = f.item_of(&t, "payable", &invoice).await;
    assert_eq!(item["due_date"], "2026-04-24");
    assert!(item["discount_date"].is_null());

    // An invoice names other terms: 30 days from the end of March.
    let order = f.order(&t, "purchase", "1", "10.00", json!({})).await;
    let invoice = f
        .bought(
            &t,
            &order,
            "2026-03-25",
            json!({ "payment_terms_id": f.eom30 }),
        )
        .await;
    assert_eq!(
        f.item_of(&t, "payable", &invoice).await["due_date"],
        "2026-04-30"
    );

    // An order names other terms than its vendor's, and its invoice takes
    // them; February ends on the 28th.
    let order = f
        .order(
            &t,
            "purchase",
            "1",
            "10.00",
            json!({ "payment_terms_id": f.eom30 }),
        )
        .await;
    assert_eq!(order["body"]["payment_terms_id"], json!(f.eom30));
    let invoice = f.bought(&t, &order, "2026-02-10", json!({})).await;
    assert_eq!(
        f.item_of(&t, "payable", &invoice).await["due_date"],
        "2026-03-30"
    );

    // The customer's 2 % 10, net 30: 119.00 owed, 2.38 off until April 4.
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(item["due_date"], "2026-04-24");
    assert_eq!(item["discount_date"], "2026-04-04");
    assert_eq!(dec(&item["discount_amount"]), 2.38);
    let _ = f.two_ten;

    // A credit note is due back at once, on its own date.
    let order = f.order(&t, "purchase", "4", "10.00", json!({})).await;
    let invoice = f.bought(&t, &order, "2026-03-25", json!({})).await;
    f.posted(
        &t,
        "purchase_return",
        "2026-03-26",
        json!({ "purchase_order_id": order["id"],
                "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "2" }] }),
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
    let (status, credit) = f.post(&t, "vendor_credit_note", &credit).await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    let item = f.item_of(&t, "payable", &credit).await;
    assert_eq!(item["due_date"], "2026-03-27");
    assert!(item["discount_date"].is_null());
    f.books(&t).await;
    t.stop().await;
}

#[tokio::test]
async fn a_discount_taken_in_time_posts_the_discount_and_takes_the_tax_back() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Bought on 2 % 10, net 30: 100.00 and 19.00 tax.
    let order = f
        .order(
            &t,
            "purchase",
            "10",
            "10.00",
            json!({ "payment_terms_id": f.two_ten }),
        )
        .await;
    let invoice = f.bought(&t, &order, "2026-03-25", json!({})).await;
    let item = f.item_of(&t, "payable", &invoice).await;
    assert_eq!(dec(&item["discount_amount"]), 2.38);

    // Paid on April 3, in time: the draft takes the discount.
    let (status, payment) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": f.c.id, "open_item_ids": [item["id"]],
                    "bank_account_id": f.c.cash, "doc_date": "2026-04-03" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{payment}");
    let allocation = &payment["body"]["allocations"][0];
    assert_eq!(dec(&allocation["amount"]), 116.62);
    assert_eq!(dec(&allocation["discount"]), 2.38);
    let (status, payment) = f.post(&t, "payment", &payment).await;
    assert_eq!(status, StatusCode::OK, "{payment}");

    // The item is settled in full; 116.62 left the bank; of the 2.38 kept
    // back, 0.38 is the tax on it, taken back from input tax, and 2.00 is
    // the discount received.
    let item = f.item_of(&t, "payable", &invoice).await;
    assert_eq!(dec(&item["txn_open"]), 0.0);
    let books = f.books(&t).await;
    assert!(close(books["2000"], 0.0));
    assert!(close(books["1000"], -116.62));
    assert!(close(books["1400"], 19.0 - 0.38));
    assert!(close(books["5040"], -2.0));
    // In the tax ledger, in the payment's period.
    let (_, report) = t
        .command(
            &t.admin,
            "tax.report",
            json!({ "company_id": f.c.id, "from": "2026-04-01", "to": "2026-04-30" }),
        )
        .await;
    assert!(close(dec(&report["total_paid"]), -0.38), "{report}");

    // Sold on the customer's 2 % 10: the same the other way round.
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    let payment = f
        .posted(
            &t,
            "payment",
            "2026-04-04",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "116.62", "discount": "2.38" }] }),
        )
        .await;
    let books = f.books(&t).await;
    assert!(close(books["1200"], 0.0));
    assert!(close(books["2300"], -(19.0 - 0.38)));
    assert!(close(books["4030"], 2.0));

    // Reversed, the payment takes the discount and the tax back with it.
    let (status, _) = t
        .command(&t.admin, "payment.reverse", json!({ "id": payment["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let books = f.books(&t).await;
    assert!(close(books["1200"], 119.0));
    assert!(close(books["2300"], -19.0));
    assert!(close(books["4030"], 0.0));

    // A discount is only for paying the rest of the item.
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-04-04",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "50", "discount": "2.38" }] }),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "discount", "{body}");
    assert!(message(&body).contains("116.62"), "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_late_discount_is_refused_unless_the_company_allows_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    let late = json!({ "direction": "incoming", "bank_account_id": f.c.cash,
        "allocations": [{ "open_item_id": item["id"], "amount": "116.62", "discount": "2.38" }] });

    // Three days after April 4: refused.
    let draft = f.draft(&t, "payment", "2026-04-07", late.clone()).await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "discount", "{body}");
    assert_eq!(body["error"]["line"], 1, "{body}");
    // Drafted after the date, a payment takes no discount.
    let (_, drafted) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": f.c.id, "open_item_ids": [item["id"]],
                    "bank_account_id": f.c.cash, "doc_date": "2026-04-07" }),
        )
        .await;
    assert_eq!(dec(&drafted["body"]["allocations"][0]["discount"]), 0.0);

    // A company that gives a week's grace, and does not correct tax for a
    // discount (as with US sales tax): the whole discount is the discount.
    for (point, condition) in [
        ("payment.late_discount", "facts.days_late <= 7"),
        ("payment.discount_tax", "true"),
    ] {
        let effect = if point == "payment.discount_tax" {
            "refuse"
        } else {
            "allow"
        };
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/policy_rule",
                json!({ "company_id": f.c.id, "point": point, "effect": effect,
                        "condition": condition, "priority": 10 }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let books = f.books(&t).await;
    assert!(close(books["1200"], 0.0));
    assert!(close(books["2300"], -19.0));
    assert!(close(books["4030"], 2.38));
    t.stop().await;
}

#[tokio::test]
async fn aging_adds_up_to_the_control_accounts_now_and_as_of_a_past_day() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Due April 24 (net 30 from March 25), 119.00.
    let order = f.order(&t, "purchase", "10", "10.00", json!({})).await;
    let first = f.bought(&t, &order, "2026-03-25", json!({})).await;
    // Due February 14 (net 30 from January 15), 59.50.
    let order = f.order(&t, "purchase", "5", "10.00", json!({})).await;
    f.bought(&t, &order, "2026-01-15", json!({})).await;
    // In euros, 23.80 at 1.10: carried at 26.18. Due May 14.
    let order = f
        .order(&t, "purchase", "2", "10.00", json!({ "currency": "EUR" }))
        .await;
    f.bought(&t, &order, "2026-04-14", json!({ "fx_rate": "1.10" }))
        .await;

    // On April 30: the January invoice is 75 days late, the March one 6,
    // the euro one current and due within 30 days.
    let aging = f.aging(&t, "payable", "2026-04-30").await;
    let labels: Vec<&str> = aging["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["current", "1-30", "31-60", "61-90", "over 90"]);
    let totals: Vec<f64> = aging["totals"]
        .as_array()
        .unwrap()
        .iter()
        .map(dec)
        .collect();
    assert_eq!(totals, [26.18, 119.0, 0.0, 59.5, 0.0]);
    assert_eq!(dec(&aging["due_next_7"]), 0.0);
    assert_eq!(dec(&aging["due_next_30"]), 26.18);
    assert_eq!(aging["parties"][0]["party_name"], "Parts Inc");
    assert_eq!(aging["accounts"][0]["account_id"], json!(f.account["2000"]));

    // Half of the March invoice is paid on May 5.
    let item = f.item_of(&t, "payable", &first).await;
    f.posted(
        &t,
        "payment",
        "2026-05-05",
        json!({ "direction": "outgoing", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item["id"], "amount": "59.50" }] }),
    )
    .await;
    let aging = f.aging(&t, "payable", "2026-05-31").await;
    assert_eq!(dec(&aging["total"]), 59.5 + 59.5 + 26.18);
    // As of April 30 nothing changed: the payment came after it.
    let past = f.aging(&t, "payable", "2026-04-30").await;
    assert_eq!(dec(&past["total"]), 119.0 + 59.5 + 26.18);
    // Before anything was posted, nothing was open.
    let before = f.aging(&t, "payable", "2026-01-01").await;
    assert_eq!(dec(&before["total"]), 0.0);
    // Other bounds.
    let (status, wider) = t
        .command(
            &t.admin,
            "open_item.aging",
            json!({ "company_id": f.c.id, "kind": "payable", "as_of": "2026-04-30", "bounds": [60] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{wider}");
    let totals: Vec<f64> = wider["totals"]
        .as_array()
        .unwrap()
        .iter()
        .map(dec)
        .collect();
    assert_eq!(totals, [26.18, 119.0, 59.5]);
    f.books(&t).await;
    t.stop().await;
}

#[tokio::test]
async fn templates_bring_terms_and_the_discount_accounts() {
    let t = TestApp::start().await;
    for (template, code) in [
        ("us_small_business", "US1"),
        ("ca_small_business", "CA1"),
        ("de_skr03", "DE1"),
        // Set up again, the shared terms are reused.
        ("us_small_business", "US2"),
    ] {
        let (status, created) = t
            .command(
                &t.admin,
                "company.setup",
                json!({ "template": template, "code": code, "name": code, "first_year": 2026 }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        for purpose in ["discount_received", "discount_given"] {
            assert_eq!(
                t.count(&format!(
                    "SELECT count(*) FROM account_rule r JOIN company c ON c.id = r.company_id
                     WHERE r.purpose = '{purpose}' AND c.code = '{code}'"
                ))
                .await,
                1,
                "{template}: {purpose}"
            );
        }
    }
    assert_eq!(
        t.count(
            "SELECT count(*) FROM payment_terms
             WHERE code IN ('DUE', 'NET30', '2-10-NET30', '2-14-NET30', 'EOM-NET30')"
        )
        .await,
        5
    );
    t.stop().await;
}

#[tokio::test]
async fn a_credit_note_after_a_discount_takes_the_discount_back_in_proportion() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Sold 10 at 10.00 with 19 % tax on 2 % 10: 119.00, paid 116.62 on
    // April 3 with 2.38 kept back (2.00 discount, 0.38 tax).
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    f.posted(
        &t,
        "payment",
        "2026-04-03",
        json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item["id"], "amount": "116.62", "discount": "2.38" }] }),
    )
    .await;

    // All ten come back, credited in two halves. Each half owes back what
    // was paid for it, 58.31 (49.00 and 9.31 tax), and takes 1.00 off the
    // discount given.
    let line = order["body"]["lines"][0]["id"].clone();
    for date in ["2026-04-05", "2026-04-06"] {
        f.posted(
            &t,
            "sales_return",
            date,
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "5" }] }),
        )
        .await;
        let (status, credit) = t
            .command(
                &t.admin,
                "sales_invoice.credit",
                json!({ "id": invoice["id"], "doc_date": date }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{credit}");
        let (status, credit) = f.post(&t, "sales_credit_note", &credit).await;
        assert_eq!(status, StatusCode::OK, "{credit}");
        let credit_item = f.item_of(&t, "receivable", &credit).await;
        assert_eq!(dec(&credit_item["amount"]), -58.31, "{credit_item}");
    }
    // What was paid is owed back; revenue, discount and tax are at zero.
    let books = f.books(&t).await;
    assert!(close(books["1200"], -116.62), "{books:?}");
    assert!(close(books["4000"], 0.0), "{books:?}");
    assert!(close(books["4030"], 0.0), "{books:?}");
    assert!(close(books["2300"], 0.0), "{books:?}");

    // Bought the same way on 2 % 10, paid with the discount, all returned
    // and credited at once: the vendor owes back the 116.62 paid.
    let order = f
        .order(
            &t,
            "purchase",
            "10",
            "10.00",
            json!({ "payment_terms_id": f.two_ten }),
        )
        .await;
    let invoice = f.bought(&t, &order, "2026-03-25", json!({})).await;
    let item = f.item_of(&t, "payable", &invoice).await;
    f.posted(
        &t,
        "payment",
        "2026-04-03",
        json!({ "direction": "outgoing", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item["id"], "amount": "116.62", "discount": "2.38" }] }),
    )
    .await;
    f.posted(
        &t,
        "purchase_return",
        "2026-04-05",
        json!({ "purchase_order_id": order["id"],
                "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "10" }] }),
    )
    .await;
    let (status, credit) = t
        .command(
            &t.admin,
            "vendor_invoice.credit",
            json!({ "id": invoice["id"], "doc_date": "2026-04-05" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    let (status, credit) = f.post(&t, "vendor_credit_note", &credit).await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    assert_eq!(
        dec(&f.item_of(&t, "payable", &credit).await["amount"]),
        -116.62
    );
    let books = f.books(&t).await;
    assert!(close(books["2000"], 116.62), "{books:?}");
    assert!(close(books["5040"], 0.0), "{books:?}");
    assert!(close(books["1400"], 0.0), "{books:?}");
    t.stop().await;
}

#[tokio::test]
async fn a_summary_of_an_older_version_is_worked_out_again() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order = f.order(&t, "purchase", "10", "10.00", json!({})).await;
    let invoice = f.bought(&t, &order, "2026-03-25", json!({})).await;
    assert_eq!(dec(&invoice["summary"]["total_gross_accounting"]), 119.0);

    // As a summary worked out before the type kept the currency and the
    // accounting totals.
    sqlx::query(
        "UPDATE document SET summary = summary - 'currency' - 'total_gross_accounting',
                summary_version = 0
         WHERE id = $1::uuid",
    )
    .bind(id(&invoice))
    .execute(&t.pool)
    .await
    .unwrap();
    let (status, done) = t
        .command(
            &t.admin,
            "document.refresh_summaries",
            json!({ "doc_type": "vendor_invoice" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["refreshed"], 1, "{done}");
    assert_eq!(done["remaining"], 0, "{done}");
    let (_, read) = t
        .get(
            &t.admin,
            &format!("/api/documents/vendor_invoice/{}", id(&invoice)),
        )
        .await;
    assert_eq!(read["summary"]["currency"], "USD", "{read}");
    assert_eq!(dec(&read["summary"]["total_gross_accounting"]), 119.0);
    // Up to date, nothing is done again.
    let (_, done) = t
        .command(
            &t.admin,
            "document.refresh_summaries",
            json!({ "doc_type": "vendor_invoice" }),
        )
        .await;
    assert_eq!(done["refreshed"], 0, "{done}");
    t.stop().await;
}

#[tokio::test]
async fn a_small_difference_is_written_off_as_far_as_the_company_allows() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let write_off = t
        .create(
            "account",
            json!({ "company_id": f.c.id, "code": "6900", "name": "Payment differences",
                    "account_type": "expense" }),
        )
        .await;
    t.create(
        "account_rule",
        json!({ "company_id": f.c.id, "purpose": "write_off", "account_id": write_off }),
    )
    .await;
    // 119.00 owed by the customer, paid short by 0.50 well after any
    // discount: the difference is written off.
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    let short = |write_off: &'static str, amount: &'static str| {
        json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item["id"], "amount": amount,
                                  "write_off": write_off }] })
    };
    // Unless the company says so, nothing is written off.
    let draft = f
        .draft(&t, "payment", "2026-04-30", short("0.50", "118.50"))
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "write_off", "{body}");
    // Up to 1.00 it is.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "payment.write_off", "effect": "allow",
                    "condition": "facts.amount <= 1", "priority": 10 }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // A write-off is what is left, no more and no less.
    let wrong = f
        .draft(&t, "payment", "2026-04-30", short("0.40", "118.50"))
        .await;
    let (status, body) = f.post(&t, "payment", &wrong).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("0.5"), "{body}");
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        dec(&f.item_of(&t, "receivable", &invoice).await["txn_open"]),
        0.0
    );
    let books = f.books(&t).await;
    assert!(close(books["1200"], 0.0));
    assert!(close(books["6900"], 0.5));

    // A vendor's invoice paid 0.10 short: written off the other way, a gain.
    let order = f.order(&t, "purchase", "10", "10.00", json!({})).await;
    let invoice = f.bought(&t, &order, "2026-03-25", json!({})).await;
    let item = f.item_of(&t, "payable", &invoice).await;
    f.posted(
        &t,
        "payment",
        "2026-04-30",
        json!({ "direction": "outgoing", "bank_account_id": f.c.cash,
                "allocations": [{ "open_item_id": item["id"], "amount": "118.90",
                                  "write_off": "0.10" }] }),
    )
    .await;
    let books = f.books(&t).await;
    assert!(close(books["2000"], 0.0));
    assert!(close(books["6900"], 0.4));
    t.stop().await;
}

#[tokio::test]
async fn terms_with_two_discount_steps_take_the_one_in_force() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // 2 % within 10 days, 1 % within 20, net 30.
    t.create(
        "payment_terms_step",
        json!({ "terms_id": f.two_ten, "discount_percent": "1", "discount_days": 20 }),
    )
    .await;
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(
        item["discount_steps"],
        json!([{ "until": "2026-04-04", "amount": "2.3800" },
               { "until": "2026-04-14", "amount": "1.1900" }])
    );
    // Drafted on April 10, the second step is offered.
    let (status, drafted) = t
        .command(
            &t.admin,
            "payment.draft",
            json!({ "company_id": f.c.id, "open_item_ids": [item["id"]],
                    "bank_account_id": f.c.cash, "doc_date": "2026-04-10" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    assert_eq!(dec(&drafted["body"]["allocations"][0]["discount"]), 1.19);
    // The first step's discount is no longer in force then.
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-04-10",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "116.62",
                                      "discount": "2.38" }] }),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("at most 1.19"), "{body}");
    let (status, body) = f.post(&t, "payment", &drafted).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let books = f.books(&t).await;
    assert!(close(books["1200"], 0.0));
    // 1.19 off: 1.00 discount and 0.19 tax back.
    assert!(close(books["4030"], 1.0));
    assert!(close(books["2300"], -(19.0 - 0.19)));
    t.stop().await;
}

/// The parts of an item, each with its due date and what is open of it.
fn parts(item: &Value) -> Vec<(String, f64)> {
    item["instalments"]
        .as_array()
        .unwrap_or_else(|| panic!("no parts: {item}"))
        .iter()
        .map(|p| (p["due_date"].as_str().unwrap().to_owned(), dec(&p["open"])))
        .collect()
}

#[tokio::test]
async fn an_invoice_due_in_parts_ages_and_is_paid_by_part() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // 30 % on the invoice's date, 30 % in 30 days, the rest in 60.
    let thirds = t
        .create(
            "payment_terms",
            json!({ "code": "PARTS", "name": "30/30/40", "net_days": 60 }),
        )
        .await;
    for (percent, days) in [("30", 0), ("30", 30)] {
        t.create(
            "payment_terms_instalment",
            json!({ "terms_id": thirds, "percent": percent, "days": days }),
        )
        .await;
    }
    // A part is due before the rest, and the parts leave something for it.
    for (percent, days) in [("10", 60), ("40", 45)] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/payment_terms_instalment",
                json!({ "terms_id": thirds, "percent": percent, "days": days }),
            )
            .await;
        assert_ne!(status, StatusCode::CREATED, "{percent} % at {days}: {body}");
    }
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/payment_terms/{thirds}"),
            json!({ "net_days": 30 }),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "net days before a part: {body}");

    // 119.00 owed: 35.70 on March 1, 35.70 on March 31, 47.60 on April 30.
    let order = f
        .order(
            &t,
            "sales",
            "10",
            "10.00",
            json!({ "payment_terms_id": thirds }),
        )
        .await;
    let invoice = f.sold(&t, &order, "2026-03-01").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(
        item["due_date"], "2026-04-30",
        "the item is due when its last part is"
    );
    assert_eq!(
        parts(&item),
        [
            ("2026-03-01".to_owned(), 35.7),
            ("2026-03-31".to_owned(), 35.7),
            ("2026-04-30".to_owned(), 47.6),
        ]
    );

    // Aging by part: on April 10 the first is 40 days late, the second
    // 10, the last falls due within 30 days.
    let aging = f.aging(&t, "receivable", "2026-04-10").await;
    let customer = &aging["parties"][0];
    assert!(close(dec(&customer["amounts"][0]), 47.6), "{aging}");
    assert!(close(dec(&customer["amounts"][1]), 35.7), "{aging}");
    assert!(close(dec(&customer["amounts"][2]), 35.7), "{aging}");
    assert!(close(dec(&customer["due_next_30"]), 47.6), "{aging}");

    // A payment that names no part pays the earliest first.
    let first = f
        .posted(
            &t,
            "payment",
            "2026-04-12",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "50.00" }] }),
        )
        .await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    let open: Vec<f64> = parts(&item).into_iter().map(|(_, o)| o).collect();
    assert_eq!(open, [0.0, 21.4, 47.6]);
    // One that names a part pays that part, and no more than is open of it.
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-04-13",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "47.61",
                                      "instalment": 3 }] }),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("of part 3"), "{body}");
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-04-13",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "47.60",
                                      "instalment": 3 }] }),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = f.item_of(&t, "receivable", &invoice).await;
    let open: Vec<f64> = parts(&item).into_iter().map(|(_, o)| o).collect();
    assert_eq!(open, [0.0, 21.4, 0.0]);
    // The ledger refuses it too, whatever the command checks.
    let paid = t
        .count(&format!(
            "SELECT count(*) FROM settlement WHERE open_item_id = '{}' AND instalment = 3",
            item["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(paid, 1);
    // A part the item does not have is refused.
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-04-13",
            json!({ "direction": "incoming", "bank_account_id": f.c.cash,
                    "allocations": [{ "open_item_id": item["id"], "amount": "1.00",
                                      "instalment": 4 }] }),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("due in 3 parts"), "{body}");

    // Taking back the first payment opens the earliest parts again.
    let (status, body) = t
        .command(&t.admin, "payment.reverse", json!({ "id": first["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = f.item_of(&t, "receivable", &invoice).await;
    let open: Vec<f64> = parts(&item).into_iter().map(|(_, o)| o).collect();
    assert_eq!(open, [35.7, 35.7, 0.0]);
    // As of April 10 nothing had been paid, and aging says so still.
    let aging = f.aging(&t, "receivable", "2026-04-10").await;
    assert!(close(dec(&aging["total"]), 119.0), "{aging}");
    assert!(
        close(dec(&aging["parties"][0]["amounts"][2]), 35.7),
        "{aging}"
    );
    // Today: two parts open, both late.
    let aging = f.aging(&t, "receivable", "2026-05-15").await;
    assert!(close(dec(&aging["total"]), 71.4), "{aging}");
    assert!(
        close(dec(&aging["parties"][0]["amounts"][0]), 0.0),
        "{aging}"
    );
    f.books(&t).await;

    // An invoice on terms without parts has none.
    let order = f.order(&t, "sales", "1", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-01").await;
    assert_eq!(
        f.item_of(&t, "receivable", &invoice).await["instalments"],
        Value::Null
    );
    t.stop().await;
}
