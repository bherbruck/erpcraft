//! Month-end revaluation of open items in a foreign currency: revalued at
//! the month end's rate, reversed the next day, and a payment after it
//! realizes its whole difference against the invoice's rate, so unrealized
//! and realized differences never count the same movement twice.

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
        .create("customer", json!({ "code": "C-EU", "name": "Kunde AG" }))
        .await;
    let mut account = BTreeMap::new();
    for (code, kind, currency) in [
        ("1010", "asset", Some("EUR")),
        ("1200", "asset", None),
        ("1300", "asset", None),
        ("2000", "liability", None),
        ("2100", "liability", None),
        ("5000", "expense", None),
        ("5200", "expense", None),
        ("7100", "income", None),
        ("7200", "expense", None),
        ("7300", "income", None),
        ("7400", "expense", None),
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
        ("fx_unrealized_gain", "7300"),
        ("fx_unrealized_loss", "7400"),
    ] {
        t.create(
            "account_rule",
            json!({ "company_id": c.id, "purpose": purpose, "account_id": account[code] }),
        )
        .await;
    }
    for (date, rate) in [
        ("2026-01-01", "1.10"),
        ("2026-03-31", "1.20"),
        ("2026-04-10", "1.15"),
        ("2026-12-31", "1.30"),
        ("2027-01-02", "1.25"),
    ] {
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
    }
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

    async fn order(&self, t: &TestApp, kind: &str, qty: &str, price: &str) -> Value {
        let mut body = match kind {
            "purchase" => json!({ "vendor_id": self.vendor }),
            _ => json!({ "customer_id": self.customer }),
        };
        body["currency"] = json!("EUR");
        body["lines"] = json!([{ "item_id": self.item, "warehouse_id": self.warehouse,
                                  "qty": qty, "unit_price": price }]);
        let draft = self
            .draft(t, &format!("{kind}_order"), "2026-03-01", body)
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

    /// Balances by account code as of a day; the books balance.
    async fn books(&self, t: &TestApp, as_of: &str) -> BTreeMap<String, f64> {
        let (_, tb) = t
            .command(
                &t.admin,
                "gl.trial_balance",
                json!({ "company_id": self.c.id, "as_of": as_of }),
            )
            .await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["code"].as_str().unwrap().to_owned(), dec(&r["balance"])))
            .collect()
    }

    /// What is open of `kind` as of a day, by aging, which also checks
    /// that it is what the control account holds.
    async fn open(&self, t: &TestApp, kind: &str, as_of: &str) -> f64 {
        let (status, aging) = t
            .command(
                &t.admin,
                "open_item.aging",
                json!({ "company_id": self.c.id, "kind": kind, "as_of": as_of }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{aging}");
        for account in aging["accounts"].as_array().unwrap() {
            assert!(
                close(dec(&account["balance"]), dec(&account["open"])),
                "{kind} as of {as_of}: {aging}"
            );
        }
        dec(&aging["total"])
    }

    async fn item(&self, t: &TestApp, kind: &str) -> Value {
        let (_, items) = t
            .command(
                &t.admin,
                "open_item.list",
                json!({ "company_id": self.c.id, "kind": kind }),
            )
            .await;
        items["items"][0].clone()
    }

    async fn revalue(&self, t: &TestApp, date: &str) -> (StatusCode, Value) {
        self.try_post(t, "fx_revaluation", date, json!({})).await
    }
}

#[tokio::test]
async fn open_items_are_revalued_at_the_month_end_and_the_payment_realizes_the_rest() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Bought 10 at 10.00 EUR and sold 5 at 20.00 EUR on March 10, at
    // 1.10: 110.00 USD owed each way.
    let buy = f.order(&t, "purchase", "10", "10.00").await;
    let buy_line = buy["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "purchase_receipt",
        "2026-03-10",
        json!({ "purchase_order_id": buy["id"], "lines": [{ "order_line_id": buy_line, "qty": "10" }] }),
    )
    .await;
    f.posted(
        &t,
        "vendor_invoice",
        "2026-03-10",
        json!({ "purchase_order_id": buy["id"],
                "lines": [{ "order_line_id": buy_line, "qty": "10", "unit_price": "10.00" }] }),
    )
    .await;
    let sell = f.order(&t, "sales", "5", "20.00").await;
    let sell_line = sell["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-10",
        json!({ "sales_order_id": sell["id"], "lines": [{ "order_line_id": sell_line, "qty": "5" }] }),
    )
    .await;
    f.posted(
        &t,
        "sales_invoice",
        "2026-03-10",
        json!({ "sales_order_id": sell["id"],
                "lines": [{ "order_line_id": sell_line, "qty": "5", "unit_price": "20.00" }] }),
    )
    .await;

    // Revalued at March 31, at 1.20: each is worth 120.00. The payable is
    // an unrealized loss of 10.00, the receivable a gain of 10.00. The next
    // day's period is open, so it is reversed on April 1 at once.
    let (status, revaluation) = f.revalue(&t, "2026-03-31").await;
    assert_eq!(status, StatusCode::OK, "{revaluation}");
    assert_eq!(revaluation["status"], "reversed", "{revaluation}");
    assert_eq!(dec(&revaluation["summary"]["total_gain"]), 10.0);
    assert_eq!(dec(&revaluation["summary"]["total_loss"]), 10.0);
    assert_eq!(revaluation["summary"]["items"], 2);
    let lines = revaluation["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|l| dec(&l["amount"]) == 10.0 && dec(&l["rate"]) == 1.2)
    );

    let march = f.books(&t, "2026-03-31").await;
    assert!(close(march["2000"], -120.0));
    assert!(close(march["1200"], 120.0));
    assert!(close(march["7400"], 10.0));
    assert!(close(march["7300"], -10.0));
    // Aging as of March 31 counts the items at their revalued value, and
    // that is what the accounts hold.
    assert_eq!(f.open(&t, "payable", "2026-03-31").await, 120.0);
    assert_eq!(f.open(&t, "receivable", "2026-03-31").await, 120.0);
    // On April 1 the items are back at the invoice's rate.
    let april = f.books(&t, "2026-04-01").await;
    assert!(close(april["2000"], -110.0));
    assert!(close(april["7400"], 0.0));
    assert!(close(april["7300"], 0.0));
    assert_eq!(f.open(&t, "payable", "2026-04-01").await, 110.0);

    // Paid on April 10 at 1.15: 115.00 for an item booked at 110.00 is a
    // realized loss of 5.00, the whole movement from the invoice. With the
    // revaluation and its reversal it is 5.00 in all, not 15.00.
    let payable = f.item(&t, "payable").await;
    f.posted(
        &t,
        "payment",
        "2026-04-10",
        json!({ "direction": "outgoing", "bank_account_id": f.account["1010"],
                "allocations": [{ "open_item_id": payable["id"], "amount": "100.00" }] }),
    )
    .await;
    let books = f.books(&t, "2026-12-31").await;
    assert!(close(books["2000"], 0.0));
    assert!(close(books["1010"], -115.0));
    assert!(close(books["7200"], 5.0), "realized");
    assert!(
        close(books["7400"] + books["7200"], 5.0),
        "realized and unrealized"
    );

    // Revalued again as of March 31, the items are already carried at
    // that day's rate there: nothing to revalue.
    let (status, body) = f.revalue(&t, "2026-03-31").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nothing to revalue"),
        "{body}"
    );
    t.stop().await;
}

#[tokio::test]
async fn a_revaluation_whose_next_period_is_not_open_waits_to_be_reversed() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let sell_stock = f.order(&t, "purchase", "5", "1.00").await;
    f.posted(
        &t,
        "purchase_receipt",
        "2026-03-10",
        json!({ "purchase_order_id": sell_stock["id"],
                "lines": [{ "order_line_id": sell_stock["body"]["lines"][0]["id"], "qty": "5" }] }),
    )
    .await;
    let sell = f.order(&t, "sales", "5", "20.00").await;
    let line = sell["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-10",
        json!({ "sales_order_id": sell["id"], "lines": [{ "order_line_id": line, "qty": "5" }] }),
    )
    .await;
    f.posted(
        &t,
        "sales_invoice",
        "2026-03-10",
        json!({ "sales_order_id": sell["id"],
                "lines": [{ "order_line_id": line, "qty": "5", "unit_price": "20.00" }] }),
    )
    .await;

    // At the year end, 1.30: 130.00 for 110.00. The company has no period
    // for 2027 yet: the revaluation stands.
    let (status, revaluation) = f.revalue(&t, "2026-12-31").await;
    assert_eq!(status, StatusCode::OK, "{revaluation}");
    assert_eq!(revaluation["status"], "posted");
    assert_eq!(dec(&revaluation["summary"]["total_gain"]), 20.0);
    assert_eq!(f.open(&t, "receivable", "2026-12-31").await, 130.0);

    // Once 2027 is open, a payment cannot settle the item while the
    // revaluation stands.
    t.create(
        "fiscal_period",
        json!({ "company_id": f.c.id, "name": "2027", "start_date": "2027-01-01", "end_date": "2027-12-31" }),
    )
    .await;
    let receivable = f.item(&t, "receivable").await;
    let payment = json!({ "direction": "incoming", "bank_account_id": f.account["1010"],
        "allocations": [{ "open_item_id": receivable["id"], "amount": "100.00" }] });
    let (status, body) = f
        .try_post(&t, "payment", "2027-01-05", payment.clone())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("fx_revaluation.reverse"),
        "{body}"
    );

    // Reversed on January 1, the item is at 110.00 again, and the payment
    // at 1.25 realizes 15.00 against that.
    let (status, reversed) = t
        .command(
            &t.admin,
            "fx_revaluation.reverse",
            json!({ "id": revaluation["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reversed}");
    assert_eq!(reversed["status"], "reversed");
    assert_eq!(f.open(&t, "receivable", "2027-01-01").await, 110.0);
    f.posted(&t, "payment", "2027-01-05", payment).await;
    let books = f.books(&t, "2027-12-31").await;
    assert!(close(books["1200"], 0.0));
    assert!(close(books["1010"], 125.0));
    assert!(close(books["7100"], -15.0), "realized");
    assert!(
        close(books["7300"], 0.0),
        "the unrealized gain was taken back"
    );
    let year_end = f.books(&t, "2026-12-31").await;
    assert!(close(year_end["7300"], -20.0), "unrealized at the year end");
    t.stop().await;
}

#[tokio::test]
async fn a_payment_the_revaluation_did_not_see_is_refused_until_it_is_reversed_before_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let stock = f.order(&t, "purchase", "5", "1.00").await;
    f.posted(
        &t,
        "purchase_receipt",
        "2026-03-10",
        json!({ "purchase_order_id": stock["id"],
                "lines": [{ "order_line_id": stock["body"]["lines"][0]["id"], "qty": "5" }] }),
    )
    .await;
    let sell = f.order(&t, "sales", "5", "20.00").await;
    let line = sell["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-10",
        json!({ "sales_order_id": sell["id"], "lines": [{ "order_line_id": line, "qty": "5" }] }),
    )
    .await;
    f.posted(
        &t,
        "sales_invoice",
        "2026-03-10",
        json!({ "sales_order_id": sell["id"],
                "lines": [{ "order_line_id": line, "qty": "5", "unit_price": "20.00" }] }),
    )
    .await;
    let receivable = f.item(&t, "receivable").await;
    let pay = |date: &'static str| {
        let t = &t;
        let f = &f;
        let item = receivable["id"].clone();
        async move {
            f.try_post(
                t,
                "payment",
                date,
                json!({ "direction": "incoming", "bank_account_id": f.account["1010"],
                        "allocations": [{ "open_item_id": item, "amount": "10.00" }] }),
            )
            .await
        }
    };

    // Revalued at March 31 and reversed at once on April 1.
    let (status, march) = f.revalue(&t, "2026-03-31").await;
    assert_eq!(status, StatusCode::OK, "{march}");
    assert_eq!(march["status"], "reversed");
    // A payment dated on the revaluation's day, or back-dated before it, was
    // not seen by it: refused while the reversal is dated after it.
    for date in ["2026-03-31", "2026-03-20"] {
        let (status, body) = pay(date).await;
        assert_eq!(status, StatusCode::CONFLICT, "{date}: {body}");
    }
    // After the reversal it goes ahead.
    let (status, body) = pay("2026-04-02").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // At the year end, with no period for 2027, the revaluation stands; it
    // is reversed later, dated January 10. A payment dated January 5 falls
    // in between and is refused; one dated January 10 is not.
    let (status, year_end) = f.revalue(&t, "2026-12-31").await;
    assert_eq!(status, StatusCode::OK, "{year_end}");
    assert_eq!(year_end["status"], "posted");
    t.create(
        "fiscal_period",
        json!({ "company_id": f.c.id, "name": "2027", "start_date": "2027-01-01", "end_date": "2027-12-31" }),
    )
    .await;
    let (status, body) = t
        .command(
            &t.admin,
            "fx_revaluation.reverse",
            json!({ "id": year_end["id"], "posting_date": "2027-01-10" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = pay("2027-01-05").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = pay("2027-01-10").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}
