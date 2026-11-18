//! Write-offs and payments of items due in parts: what is left written off
//! without money, of a debt or a credit, of the item or of one part;
//! several parts paid in one payment; what someone who may not read open
//! items is told; and payments that race for the same part.

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
    let net30 = t
        .create(
            "payment_terms",
            json!({ "code": "NET30", "name": "Net 30", "net_days": 30 }),
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

async fn write_off_account(t: &TestApp, f: &Fixture) {
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
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "payment.write_off", "effect": "allow",
                    "condition": "facts.amount <= 1", "priority": 10 }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 30/30/40 terms: 35.70, 35.70 and 47.60 of an invoice of 119.00.
async fn parts_terms(t: &TestApp) -> String {
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
    thirds
}

/// What is open of each part of an item.
fn parts_open(item: &Value) -> Vec<f64> {
    item["instalments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| dec(&p["open"]))
        .collect()
}

/// A payment of `allocations`, incoming, through the cash account.
fn incoming(f: &Fixture, allocations: Value) -> Value {
    json!({ "direction": "incoming", "bank_account_id": f.c.cash, "allocations": allocations })
}

/// A customer paid 118.50 of 119.00; the 0.50 left is written off later
/// with no money, as far as the company's tolerance (1.00) goes. Beyond it
/// the policy refuses, and nothing else does.
#[tokio::test]
async fn what_is_left_is_written_off_without_money_as_far_as_the_policy_allows() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    write_off_account(&t, &f).await;
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    f.posted(
        &t,
        "payment",
        "2026-04-30",
        incoming(
            &f,
            json!([{ "open_item_id": item["id"], "amount": "118.50" }]),
        ),
    )
    .await;
    let write_off = f
        .posted(
            &t,
            "payment",
            "2026-05-31",
            incoming(
                &f,
                json!([{ "open_item_id": item["id"], "amount": "0", "write_off": "0.50" }]),
            ),
        )
        .await;
    assert_eq!(dec(&write_off["summary"]["total"]), 0.0, "{write_off}");
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert!(close(dec(&item["txn_open"]), 0.0), "{item}");
    let books = f.books(&t).await;
    assert!(close(books["6900"], 0.5), "{books:?}");

    // 2.00 left is more than the company writes off.
    let invoice = f.sold(&t, &order_of(&t, &f).await, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    f.posted(
        &t,
        "payment",
        "2026-04-30",
        incoming(
            &f,
            json!([{ "open_item_id": item["id"], "amount": "117.00" }]),
        ),
    )
    .await;
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-05-31",
            incoming(
                &f,
                json!([{ "open_item_id": item["id"], "amount": "0", "write_off": "2.00" }]),
            ),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "write_off", "{body}");
    // Nothing paid and nothing written off is still nothing.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({ "company_id": f.c.id, "doc_date": "2026-05-31",
                    "body": incoming(&f, json!([{ "open_item_id": item["id"], "amount": "0" }])) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    f.books(&t).await;
    t.stop().await;
}

/// Another approved sales order of 10 at 10.00.
async fn order_of(t: &TestApp, f: &Fixture) -> Value {
    f.order(t, "sales", "10", "10.00", json!({})).await
}

/// A customer overpaid by 0.03, which stays as a credit: it is written off
/// rather than refunded, with no money, as income on the write-off account.
#[tokio::test]
async fn a_small_credit_is_written_off() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    write_off_account(&t, &f).await;
    let order = f.order(&t, "sales", "10", "10.00", json!({})).await;
    let invoice = f.sold(&t, &order, "2026-03-25").await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    let mut body = incoming(
        &f,
        json!([{ "open_item_id": item["id"], "amount": "119.00" }]),
    );
    body["on_account"] = json!("0.03");
    let overpaid = f.posted(&t, "payment", "2026-04-30", body).await;
    let credit = f.item_of(&t, "receivable", &overpaid).await;
    assert!(close(dec(&credit["txn_open"]), -0.03), "{credit}");
    f.posted(
        &t,
        "payment",
        "2026-05-31",
        incoming(
            &f,
            json!([{ "open_item_id": credit["id"], "amount": "0", "write_off": "0.03" }]),
        ),
    )
    .await;
    let credit = f.item_of(&t, "receivable", &overpaid).await;
    assert!(close(dec(&credit["txn_open"]), 0.0), "{credit}");
    let books = f.books(&t).await;
    assert!(close(books["6900"], -0.03), "{books:?}");
    assert!(close(books["1200"], 0.0), "{books:?}");
    t.stop().await;
}

/// A part paid a few cents short is written off with its payment, before
/// the last part is paid; the other parts stay as they were.
#[tokio::test]
async fn a_part_paid_short_is_written_off_before_the_last_part() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    write_off_account(&t, &f).await;
    let thirds = parts_terms(&t).await;
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
    // What is left of the part, not of the item, is what is written off.
    let draft = f
        .draft(
            &t,
            "payment",
            "2026-03-02",
            incoming(
                &f,
                json!([{ "open_item_id": item["id"], "amount": "35.50", "write_off": "0.30",
                         "instalment": 1 }]),
            ),
        )
        .await;
    let (status, body) = f.post(&t, "payment", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("left of part 1"), "{body}");
    f.posted(
        &t,
        "payment",
        "2026-03-02",
        incoming(
            &f,
            json!([{ "open_item_id": item["id"], "amount": "35.50", "write_off": "0.20",
                     "instalment": 1 }]),
        ),
    )
    .await;
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(parts_open(&item), vec![0.0, 35.7, 47.6], "{item}");
    assert!(close(dec(&item["txn_open"]), 83.3), "{item}");
    let books = f.books(&t).await;
    assert!(close(books["6900"], 0.2), "{books:?}");
    f.aging(&t, "receivable", "2026-03-02").await;
    t.stop().await;
}

/// One payment pays two parts of one item, each by name. The same part
/// twice, or a part beside the item unnamed, is refused.
#[tokio::test]
async fn several_parts_are_paid_in_one_payment() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let thirds = parts_terms(&t).await;
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
    for twice in [
        json!([{ "open_item_id": item["id"], "amount": "10.00", "instalment": 1 },
               { "open_item_id": item["id"], "amount": "10.00", "instalment": 1 }]),
        json!([{ "open_item_id": item["id"], "amount": "10.00", "instalment": 2 },
               { "open_item_id": item["id"], "amount": "10.00" }]),
    ] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/documents/payment",
                json!({ "company_id": f.c.id, "doc_date": "2026-03-02",
                        "body": incoming(&f, twice) }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(message(&body).contains("each naming its part"), "{body}");
    }
    let payment = f
        .posted(
            &t,
            "payment",
            "2026-03-02",
            incoming(
                &f,
                json!([{ "open_item_id": item["id"], "amount": "35.70", "instalment": 1 },
                       { "open_item_id": item["id"], "amount": "35.70", "instalment": 2 }]),
            ),
        )
        .await;
    assert_eq!(dec(&payment["summary"]["total"]), 71.4, "{payment}");
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(parts_open(&item), vec![0.0, 0.0, 47.6], "{item}");
    f.books(&t).await;
    // Reversed, both parts are open again.
    let (status, body) = t
        .command(&t.admin, "payment.reverse", json!({ "id": payment["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(parts_open(&item), vec![35.7, 35.7, 47.6], "{item}");
    f.books(&t).await;
    t.stop().await;
}

/// Someone who posts payments but may not read open items is told that a
/// write-off, a part or a discount does not fit, but not what is open or
/// offered; someone who may read them is told.
#[tokio::test]
async fn a_cashier_is_not_told_what_is_open() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    write_off_account(&t, &f).await;
    let thirds = parts_terms(&t).await;
    let order = f
        .order(
            &t,
            "sales",
            "10",
            "10.00",
            json!({ "payment_terms_id": thirds }),
        )
        .await;
    let in_parts = f.sold(&t, &order, "2026-03-01").await;
    let in_parts = f.item_of(&t, "receivable", &in_parts).await;
    // The customer's own terms: 2 % within 10 days, 2.38 of 119.00.
    let with_discount = f.sold(&t, &order_of(&t, &f).await, "2026-03-25").await;
    let with_discount = f.item_of(&t, "receivable", &with_discount).await;
    let cashier = t
        .user_with_role(
            "cashier@example.com",
            &["payment:read", "payment:create", "payment:post"],
            &f.c.id,
        )
        .await;
    let tries = [
        json!({ "open_item_id": in_parts["id"], "amount": "1.00", "write_off": "0.01" }),
        json!({ "open_item_id": in_parts["id"], "amount": "40.00", "instalment": 1 }),
        json!({ "open_item_id": with_discount["id"], "amount": "100.00", "discount": "2.38" }),
        json!({ "open_item_id": with_discount["id"], "amount": "100.00", "discount": "5.00" }),
    ];
    let told = ["118", "35.7", "116.62", "2.38"];
    for (who, sees) in [(&cashier, false), (&t.admin, true)] {
        let mut leaked = vec![];
        for allocation in &tries {
            let (status, draft) = t
                .post(
                    who,
                    "/api/documents/payment",
                    json!({ "company_id": f.c.id, "doc_date": "2026-03-30",
                            "body": incoming(&f, json!([allocation])) }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            let (status, body) = t
                .command(who, "payment.post", json!({ "id": id(&draft) }))
                .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            let text = message(&body).to_owned();
            if told
                .iter()
                .any(|amount| text.contains(amount) && !allocation.to_string().contains(amount))
            {
                leaked.push(text);
            }
        }
        if sees {
            assert_eq!(leaked.len(), tries.len(), "{leaked:?}");
        } else {
            assert!(
                leaked.is_empty(),
                "open amounts told to a cashier: {leaked:?}"
            );
        }
    }
    t.stop().await;
}

/// A payment naming part 1 waits while an unnamed payment holds the item.
/// The unnamed one pays part 1; the named one, reading the parts once it
/// has the item, finds nothing open of part 1 and is refused.
#[tokio::test]
async fn a_named_part_is_read_after_the_payment_that_held_the_item() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let thirds = parts_terms(&t).await;
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
    let unnamed = f
        .draft(
            &t,
            "payment",
            "2026-03-02",
            incoming(
                &f,
                json!([{ "open_item_id": item["id"], "amount": "35.70" }]),
            ),
        )
        .await;
    let named = f
        .draft(
            &t,
            "payment",
            "2026-03-02",
            incoming(
                &f,
                json!([{ "open_item_id": item["id"], "amount": "35.70", "instalment": 1 }]),
            ),
        )
        .await;
    // Hold the item, so both queue for it, the unnamed one first.
    let mut hold = t.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM open_item WHERE id = $1::uuid FOR UPDATE")
        .bind(item["id"].as_str().unwrap())
        .execute(&mut *hold)
        .await
        .unwrap();
    let wait = |ms| tokio::time::sleep(std::time::Duration::from_millis(ms));
    let ((su, bu), (sn, bn), ()) = tokio::join!(
        f.post(&t, "payment", &unnamed),
        async {
            wait(400).await;
            f.post(&t, "payment", &named).await
        },
        async {
            wait(1200).await;
            hold.commit().await.unwrap();
        },
    );
    assert_eq!(su, StatusCode::OK, "{bu}");
    assert_eq!(sn, StatusCode::UNPROCESSABLE_ENTITY, "{bn}");
    assert!(
        message(&bn).contains("only 0 is still open of part 1"),
        "refused by the payment's own check, on the parts as they are now: {bn}"
    );
    let item = f.item_of(&t, "receivable", &invoice).await;
    assert_eq!(parts_open(&item), vec![0.0, 35.7, 47.6], "{item}");
    f.books(&t).await;
    t.stop().await;
}

/// Two payments of the same part at once: one is posted, the other
/// refused as a validation error, and the books still equal the items.
#[tokio::test]
async fn two_payments_of_one_part_at_once_post_one() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let thirds = parts_terms(&t).await;
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
    let body = incoming(
        &f,
        json!([{ "open_item_id": item["id"], "amount": "35.70", "instalment": 1 }]),
    );
    let a = f.draft(&t, "payment", "2026-03-02", body.clone()).await;
    let b = f.draft(&t, "payment", "2026-03-02", body).await;
    let ((sa, ba), (sb, bb)) = tokio::join!(f.post(&t, "payment", &a), f.post(&t, "payment", &b));
    let ok = [sa, sb].iter().filter(|s| **s == StatusCode::OK).count();
    assert_eq!(ok, 1, "{sa} {ba} / {sb} {bb}");
    assert!(
        [sa, sb].contains(&StatusCode::UNPROCESSABLE_ENTITY),
        "{sa} {ba} / {sb} {bb}"
    );
    f.books(&t).await;
    t.stop().await;
}

/// Parts added to the same terms at once cannot take them to 100 %: the
/// second waits for the first and counts it.
#[tokio::test]
async fn parts_added_at_once_still_leave_something_for_the_last() {
    let t = TestApp::start().await;
    let terms = t
        .create(
            "payment_terms",
            json!({ "code": "HALVES", "name": "Halves", "net_days": 60 }),
        )
        .await;
    let add = |days: i64| {
        t.post(
            &t.admin,
            "/api/entities/payment_terms_instalment",
            json!({ "terms_id": terms, "percent": "60", "days": days }),
        )
    };
    let ((sa, ba), (sb, bb)) = tokio::join!(add(10), add(20));
    let created = [sa, sb]
        .iter()
        .filter(|s| **s == StatusCode::CREATED)
        .count();
    assert_eq!(created, 1, "{sa} {ba} / {sb} {bb}");
    let total: i64 = t
        .count(&format!(
            "SELECT count(*) FROM payment_terms_instalment WHERE terms_id = '{terms}'"
        ))
        .await;
    assert_eq!(total, 1);
    t.stop().await;
}
