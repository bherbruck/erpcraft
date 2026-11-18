//! More than one tax on a line: a tax group made of component codes, each
//! with its own rate and accounts, owed to its own authority. Canada's GST
//! with a provincial sales tax, a US state with a county, and compounding.
//! The books are checked at every step: the trial balance balances, the
//! tax report equals the tax accounts, and open items equal the control
//! accounts.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

const MARCH: &str = "2026-03-15";
const APRIL: &str = "2026-04-10";

/// A company set up from a template, with an item, a vendor and a
/// customer, and ten widgets in stock.
struct Shop {
    company: String,
    item: String,
    vendor: String,
    customer: String,
    warehouse: Value,
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn shop(t: &TestApp, template: &str, code: &str) -> Shop {
    let created = run(
        t,
        "company.setup",
        json!({ "template": template, "code": code, "name": format!("{code} Ltd"),
                "first_year": 2026 }),
    )
    .await;
    assert_eq!(created["notes"], json!([]), "{created}");
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let unique = |what: &str| format!("{what}-{code}");
    Shop {
        item: t
            .create(
                "item",
                json!({ "code": unique("WIDGET"), "name": "Widget" }),
            )
            .await,
        vendor: t
            .create(
                "vendor",
                json!({ "code": unique("V"), "name": "Parts Inc" }),
            )
            .await,
        customer: t
            .create(
                "customer",
                json!({ "code": unique("C"), "name": "Buyer Ltd" }),
            )
            .await,
        warehouse: created["warehouse_id"].clone(),
        company,
    }
}

async fn tax_code(t: &TestApp, code: &str) -> String {
    let (_, list) = t
        .get(&t.admin, &format!("/api/entities/tax_code?code={code}"))
        .await;
    list["items"][0]["id"].as_str().unwrap().to_owned()
}

impl Shop {
    /// Draft a document and run `command` on it; returns the answer.
    async fn try_post(
        &self,
        t: &TestApp,
        doc_type: &str,
        date: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.company, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
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
        body["lines"] = json!([{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }]);
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{kind}_order"),
                json!({ "company_id": self.company, "doc_date": MARCH, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(
            t,
            &format!("{kind}_order.approve"),
            json!({ "id": id(&draft) }),
        )
        .await
    }

    /// Buy `qty` widgets at `price`, receive them, and be invoiced with
    /// `tax`. Returns the invoice.
    async fn buy(&self, t: &TestApp, qty: &str, price: &str, tax: &str) -> Value {
        let order = self.order(t, "purchase", qty, price).await;
        let line = &order["body"]["lines"][0]["id"];
        self.posted(
            t,
            "purchase_receipt",
            MARCH,
            json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        self.posted(
            t,
            "vendor_invoice",
            MARCH,
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price, "tax_code_id": tax }] }),
        )
        .await
    }

    /// Sell, deliver and invoice with `tax`. Returns the order and the
    /// invoice, or the refusal of the invoice.
    async fn sell(
        &self,
        t: &TestApp,
        qty: &str,
        price: &str,
        tax: &str,
    ) -> (Value, (StatusCode, Value)) {
        let order = self.order(t, "sales", qty, price).await;
        let line = &order["body"]["lines"][0]["id"];
        self.posted(
            t,
            "sales_delivery",
            MARCH,
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": qty }] }),
        )
        .await;
        let invoice = self
            .try_post(
                t,
                "sales_invoice",
                MARCH,
                json!({ "sales_order_id": order["id"],
                        "lines": [{ "order_line_id": line, "qty": qty, "unit_price": price, "tax_code_id": tax }] }),
            )
            .await;
        (order, invoice)
    }

    /// The balances by account code; on the way the books are checked:
    /// they balance, and open items equal the payables and receivables.
    async fn books(&self, t: &TestApp) -> BTreeMap<String, f64> {
        let tb = run(t, "gl.trial_balance", json!({ "company_id": self.company })).await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        let balances: BTreeMap<String, f64> = tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["code"].as_str().unwrap().to_owned(), dec(&r["balance"])))
            .collect();
        let at = |code: &str| balances.get(code).copied().unwrap_or(0.0);
        for (kind, account, sign) in [("payable", "2010", -1.0), ("receivable", "1200", 1.0)] {
            let items = run(
                t,
                "open_item.list",
                json!({ "company_id": self.company, "kind": kind }),
            )
            .await;
            assert!(
                (dec(&items["total_open"]) - sign * at(account)).abs() < 1e-9,
                "open {kind}s equal account {account}"
            );
        }
        balances
    }

    /// The tax report for a range, by code: (collected, paid, collected
    /// base, paid base).
    async fn report(&self, t: &TestApp, from: &str, to: &str) -> BTreeMap<String, [f64; 4]> {
        let report = run(
            t,
            "tax.report",
            json!({ "company_id": self.company, "from": from, "to": to }),
        )
        .await;
        report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["code"].as_str().unwrap().to_owned(),
                    [
                        dec(&r["collected"]),
                        dec(&r["paid"]),
                        dec(&r["collected_base"]),
                        dec(&r["paid_base"]),
                    ],
                )
            })
            .collect()
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[tokio::test]
async fn british_columbia_charges_gst_and_pst_on_one_line() {
    let t = TestApp::start().await;
    let s = shop(&t, "ca_small_business", "BC").await;
    let bc = tax_code(&t, "CA-GST-PST-BC").await;

    // Bought: 40.00 net, GST 2.00 claimed back, PST 2.80 an expense.
    let bought = s.buy(&t, "10", "4.00", &bc).await;
    assert_eq!(dec(&bought["body"]["lines"][0]["tax_amount"]), 4.8);
    assert_eq!(dec(&bought["summary"]["total_tax"]), 4.8);
    // Sold: 36.00 net, GST 1.80 and PST 2.52, each owed to its own
    // authority. The draft's summary said what was posted.
    let (_, (status, sold)) = s.sell(&t, "4", "9.00", &bc).await;
    assert_eq!(status, StatusCode::OK, "{sold}");
    assert_eq!(dec(&sold["summary"]["total_tax"]), 4.32);
    assert_eq!(dec(&sold["summary"]["total_gross"]), 40.32);
    let tax = &sold["postings"][0]["entries"]["tax"];
    assert_eq!(tax.as_array().unwrap().len(), 2, "{tax}");

    let books = s.books(&t).await;
    assert!(close(books["1350"], 2.0), "GST claimed back");
    assert!(close(books["6130"], 2.8), "PST paid is an expense");
    assert!(close(books["2100"], -1.8), "GST owed");
    assert!(close(books["2110"], -2.52), "BC PST owed");
    assert!(close(books["1200"], 40.32));
    assert!(close(books["2010"], -44.8));
    // One row per authority's code, none for the group; the report equals
    // the tax accounts.
    let report = s.report(&t, "2026-01-01", "2026-12-31").await;
    assert_eq!(report.keys().collect::<Vec<_>>(), ["CA-GST", "CA-PST-BC"]);
    assert_eq!(report["CA-GST"], [1.8, 2.0, 36.0, 40.0]);
    assert_eq!(report["CA-PST-BC"], [2.52, 2.8, 36.0, 40.0]);
    assert!(close(
        report.values().map(|r| r[0]).sum::<f64>(),
        -(books["2100"] + books["2110"])
    ));
    assert!(close(
        report.values().map(|r| r[1]).sum::<f64>(),
        books["1350"] + books["6130"]
    ));

    // Two come back in April and are credited: each tax is taken back from
    // its own account, in April.
    let invoice = sold;
    let order_line = &invoice["body"]["lines"][0];
    let order = invoice["body"]["sales_order_id"].clone();
    let (_, order_doc) = t
        .get(
            &t.admin,
            &format!("/api/documents/sales_order/{}", order.as_str().unwrap()),
        )
        .await;
    s.posted(
        &t,
        "sales_return",
        APRIL,
        json!({ "sales_order_id": order, "lines": [{ "order_line_id": order_doc["body"]["lines"][0]["id"], "qty": "2" }] }),
    )
    .await;
    let credit = s
        .posted(
            &t,
            "sales_credit_note",
            APRIL,
            json!({ "sales_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": order_line["id"], "qty": "2" }] }),
        )
        .await;
    assert_eq!(dec(&credit["summary"]["total_tax"]), 2.16);
    let april = s.report(&t, "2026-04-01", "2026-04-30").await;
    assert_eq!(april["CA-GST"], [-0.9, 0.0, -18.0, 0.0]);
    assert_eq!(april["CA-PST-BC"], [-1.26, 0.0, -18.0, 0.0]);
    // The other two as well: everything charged comes back, to the cent.
    s.posted(
        &t,
        "sales_return",
        APRIL,
        json!({ "sales_order_id": order, "lines": [{ "order_line_id": order_doc["body"]["lines"][0]["id"], "qty": "2" }] }),
    )
    .await;
    s.posted(
        &t,
        "sales_credit_note",
        APRIL,
        json!({ "sales_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": order_line["id"], "qty": "2" }] }),
    )
    .await;
    let books = s.books(&t).await;
    assert!(close(books.get("2100").copied().unwrap_or(0.0), 0.0));
    assert!(close(books.get("2110").copied().unwrap_or(0.0), 0.0));
    let year = s.report(&t, "2026-01-01", "2026-12-31").await;
    assert_eq!(year["CA-GST"][0], 0.0);
    assert_eq!(year["CA-PST-BC"][0], 0.0);

    // An inactive component refuses the group, by default.
    let pst = tax_code(&t, "CA-PST-BC").await;
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/tax_code/{pst}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, (status, refused)) = s.sell(&t, "1", "9.00", &bc).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "tax code CA-PST-BC is inactive"
    );
    t.stop().await;
}

#[tokio::test]
async fn quebec_charges_qst_beside_the_gst_and_gives_it_back_on_purchases() {
    let t = TestApp::start().await;
    let s = shop(&t, "ca_small_business", "QC").await;
    let qc = tax_code(&t, "CA-GST-QST").await;
    s.buy(&t, "10", "4.00", &qc).await;
    let (_, (status, sold)) = s.sell(&t, "4", "9.00", &qc).await;
    assert_eq!(status, StatusCode::OK, "{sold}");
    // QST is on the price without the GST: 36.00 × 9.975 % = 3.591, 3.59.
    assert_eq!(dec(&sold["summary"]["total_tax"]), 5.39);
    let books = s.books(&t).await;
    assert!(close(books["2140"], -3.59), "QST owed");
    assert!(
        close(books["1360"], 3.99),
        "QST paid is an input tax refund"
    );
    assert!(close(books["1350"], 2.0));
    assert!(
        close(books.get("6130").copied().unwrap_or(0.0), 0.0),
        "nothing expensed"
    );
    let report = s.report(&t, "2026-01-01", "2026-12-31").await;
    assert_eq!(report["CA-QST"], [3.59, 3.99, 36.0, 40.0]);
    t.stop().await;
}

#[tokio::test]
async fn a_us_sale_carries_state_and_county_tax() {
    let t = TestApp::start().await;
    let s = shop(&t, "us_small_business", "US").await;
    let group = tax_code(&t, "US-EXAMPLE-STATE-COUNTY").await;
    s.buy(&t, "10", "4.00", &group).await;
    let (_, (status, sold)) = s.sell(&t, "4", "9.00", &group).await;
    assert_eq!(status, StatusCode::OK, "{sold}");
    assert_eq!(dec(&sold["summary"]["total_tax"]), 1.98);
    let books = s.books(&t).await;
    assert!(close(books["2100"], -1.44), "state tax owed");
    assert!(close(books["2110"], -0.54), "county tax owed");
    assert!(close(books["6130"], 2.2), "sales tax paid is an expense");
    let report = s.report(&t, "2026-01-01", "2026-12-31").await;
    assert_eq!(report["US-EXAMPLE-STATE"][0], 1.44);
    assert_eq!(report["US-EXAMPLE-COUNTY"][0], 0.54);
    t.stop().await;
}

#[tokio::test]
async fn a_compounding_component_is_charged_on_the_taxes_before_it() {
    let t = TestApp::start().await;
    let s = shop(&t, "us_small_business", "CMP").await;
    let (_, accounts) = t
        .get(
            &t.admin,
            &format!("/api/entities/account?company_id={}&limit=500", s.company),
        )
        .await;
    let account = |code: &str| {
        accounts["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["code"] == code)
            .unwrap()["id"]
            .clone()
    };
    let first = t
        .create(
            "tax_code",
            json!({ "code": "FIRST", "name": "First 5 %", "rate": "5" }),
        )
        .await;
    let second = t
        .create(
            "tax_code",
            json!({ "code": "SECOND", "name": "Second 10 % on first", "rate": "10", "compound": true }),
        )
        .await;
    for (code, collected) in [(&first, "2100"), (&second, "2110")] {
        t.create(
            "tax_settings",
            json!({ "company_id": s.company, "tax_code_id": code,
                    "collected_account_id": account(collected), "paid_account_id": account("6130") }),
        )
        .await;
    }
    let group = t
        .create(
            "tax_code",
            json!({ "code": "BOTH", "name": "Both", "rate": "15" }),
        )
        .await;
    for (position, component) in [(1, &first), (2, &second)] {
        t.create(
            "tax_code_component",
            json!({ "group_id": group, "component_id": component, "position": position }),
        )
        .await;
    }
    // A group cannot be a component, nor a component a group.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/tax_code_component",
            json!({ "group_id": first, "component_id": second, "position": 1 }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED);

    s.buy(&t, "10", "4.00", &group).await;
    // 100.00 net: the first 5.00, the second 10 % of 105.00, 10.50.
    let (order, (status, sold)) = s.sell(&t, "10", "10.00", &group).await;
    assert_eq!(status, StatusCode::OK, "{sold}");
    assert_eq!(dec(&sold["summary"]["total_tax"]), 15.5);
    let books = s.books(&t).await;
    assert!(close(books["2100"], -5.0));
    assert!(close(books["2110"], -10.5));
    let report = s.report(&t, "2026-01-01", "2026-12-31").await;
    assert_eq!(report["SECOND"], [10.5, 4.2, 105.0, 42.0]);

    // Half credited: the first gives back 2.50, the second 10 % of 52.50.
    s.posted(
        &t,
        "sales_return",
        APRIL,
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "5" }] }),
    )
    .await;
    let credit = s
        .posted(
            &t,
            "sales_credit_note",
            APRIL,
            json!({ "sales_invoice_id": sold["id"], "lines": [{ "invoice_line_id": sold["body"]["lines"][0]["id"], "qty": "5" }] }),
        )
        .await;
    assert_eq!(dec(&credit["summary"]["total_tax"]), 7.75);
    let april = s.report(&t, "2026-04-01", "2026-04-30").await;
    assert_eq!(april["FIRST"][0], -2.5);
    assert_eq!(april["SECOND"], [-5.25, 0.0, -52.5, 0.0]);
    s.books(&t).await;
    t.stop().await;
}
