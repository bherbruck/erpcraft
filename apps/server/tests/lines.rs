//! Line discounts, charges and lines that are not stock on orders,
//! invoices and credit notes, with the books checked at every step: totals
//! are what is posted, the books balance, open items equal the control
//! accounts and the tax report equals the tax accounts.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

struct Fixture {
    c: Company,
    item: String,
    warehouse: String,
    customer: String,
    vendor: String,
    account: BTreeMap<&'static str, String>,
    vat: String,
    freight: String,
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
    let customer = t
        .create("customer", json!({ "code": "C1", "name": "Buyer Ltd" }))
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Parts Inc" }))
        .await;
    let mut account = BTreeMap::new();
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("1400", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("2300", "liability"),
        ("4100", "income"),
        ("4200", "income"),
        ("5000", "expense"),
        ("5200", "expense"),
        ("6100", "expense"),
    ] {
        account.insert(
            code,
            t.create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind }),
            )
            .await,
        );
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
    let freight = t
        .create(
            "charge",
            json!({ "code": "FREIGHT", "name": "Freight", "kind": "freight", "tax_code_id": vat }),
        )
        .await;
    // Freight billed to customers is income of its own.
    t.create(
        "account_rule",
        json!({ "company_id": c.id, "purpose": "charge_revenue", "scope_type": "charge",
                "scope_id": freight, "account_id": account["4100"] }),
    )
    .await;
    Fixture {
        c,
        item,
        warehouse,
        customer,
        vendor,
        account,
        vat,
        freight,
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

    async fn approved(&self, t: &TestApp, doc_type: &str, body: Value) -> Value {
        let draft = self.draft(t, doc_type, "2026-03-01", body).await;
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

    /// Stock to sell: 100 bought at 1.00.
    async fn stock(&self, t: &TestApp) {
        let order = self
            .approved(
                t,
                "purchase_order",
                json!({ "vendor_id": self.vendor,
                        "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse,
                                    "qty": "100", "unit_price": "1.00" }] }),
            )
            .await;
        self.posted(
            t,
            "purchase_receipt",
            "2026-03-01",
            json!({ "purchase_order_id": order["id"],
                    "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "100" }] }),
        )
        .await;
    }

    async fn now(&self, t: &TestApp, doc_type: &str, doc: &Value) -> Value {
        let (_, read) = t
            .get(&t.admin, &format!("/api/documents/{doc_type}/{}", id(doc)))
            .await;
        read
    }

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
}

#[tokio::test]
async fn sales_discounts_round_per_line_and_the_totals_are_what_is_posted() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock(&t).await;

    // 3 at 3.33 less 10 %: 9.99 less 1.00 (0.999 rounded) is 8.99. 7 at
    // 1.15 less 0.50: 8.05 less 0.50 is 7.55. Net 16.54.
    let order = f
        .approved(
            &t,
            "sales_order",
            json!({ "customer_id": f.customer, "lines": [
                { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "3",
                  "unit_price": "3.33", "discount_percent": "10" },
                { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "7",
                  "unit_price": "1.15", "discount_amount": "0.50" } ] }),
        )
        .await;
    assert_eq!(dec(&order["summary"]["total_net"]), 16.54);
    // 1.00 and 0.50 off; no charges.
    assert_eq!(dec(&order["summary"]["total_discount"]), 1.5);
    assert_eq!(dec(&order["summary"]["total_charges"]), 0.0);
    let lines = order["body"]["lines"].as_array().unwrap().clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-10",
        json!({ "sales_order_id": order["id"], "lines": [
            { "order_line_id": lines[0]["id"], "qty": "3" },
            { "order_line_id": lines[1]["id"], "qty": "7" } ] }),
    )
    .await;

    // The invoice takes the order's discounts; tax is per line on the net:
    // 1.7081 is 1.71, 1.4345 is 1.43.
    let (status, draft) = t
        .command(
            &t.admin,
            "sales_order.invoice",
            json!({ "id": order["id"], "doc_date": "2026-03-12" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let mut body = draft["body"].clone();
    for line in body["lines"].as_array_mut().unwrap() {
        line["tax_code_id"] = json!(f.vat);
    }
    let (status, draft) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_invoice/{}", id(&draft)),
            json!({ "body": body, "version": draft["version"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(dec(&draft["body"]["lines"][0]["discount_percent"]), 10.0);
    assert_eq!(dec(&draft["body"]["lines"][1]["discount_amount"]), 0.5);
    let (status, invoice) = f.post(&t, "sales_invoice", &draft).await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    assert_eq!(dec(&invoice["summary"]["total_net"]), 16.54);
    assert_eq!(dec(&invoice["summary"]["total_tax"]), 314.0 / 100.0);
    assert_eq!(dec(&invoice["summary"]["total_gross"]), 19.68);
    assert_eq!(dec(&invoice["summary"]["total_discount"]), 1.5);
    assert_eq!(dec(&invoice["body"]["lines"][0]["net_amount"]), 8.99);
    let books = f.books(&t).await;
    assert!(close(books["1200"], 19.68));
    assert!(close(books["4000"], -16.54));
    assert!(close(books["2300"], -314.0 / 100.0));

    // A credit for the first line takes back its discounted net and tax.
    f.posted(
        &t,
        "sales_return",
        "2026-03-15",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": lines[0]["id"], "qty": "3" }] }),
    )
    .await;
    let (status, credit) = t
        .command(
            &t.admin,
            "sales_invoice.credit",
            json!({ "id": invoice["id"], "doc_date": "2026-03-15" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    let (status, credit) = f.post(&t, "sales_credit_note", &credit).await;
    assert_eq!(status, StatusCode::OK, "{credit}");
    assert_eq!(dec(&credit["summary"]["total_net"]), 8.99);
    assert_eq!(dec(&credit["summary"]["total_gross"]), 10.70);
    let books = f.books(&t).await;
    assert!(close(books["4000"], -7.55));

    // Reversed, the invoice is gone from the books with its discounts;
    // first the credit note.
    for (doc_type, doc) in [("sales_credit_note", &credit), ("sales_invoice", &invoice)] {
        let (status, body) = t
            .command(
                &t.admin,
                &format!("{doc_type}.reverse"),
                json!({ "id": doc["id"] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let books = f.books(&t).await;
    assert!(close(books["1200"], 0.0));
    assert!(close(books["4000"], 0.0));
    t.stop().await;
}

#[tokio::test]
async fn a_freight_charge_on_a_sales_invoice_is_taxed_and_posted_to_its_account() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock(&t).await;
    let order = f
        .approved(
            &t,
            "sales_order",
            json!({ "customer_id": f.customer, "lines": [
                { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "10", "unit_price": "5.00" } ] }),
        )
        .await;
    let line = order["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-10",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
    )
    .await;
    // Freight of 12.00 on the invoice, a line of its own: taxed with the
    // charge's code, posted to the freight income account.
    let invoice = f
        .posted(
            &t,
            "sales_invoice",
            "2026-03-12",
            json!({ "sales_order_id": order["id"], "lines": [
                { "order_line_id": line, "qty": "10", "unit_price": "5.00", "tax_code_id": f.vat },
                { "charge_id": f.freight, "qty": "1", "unit_price": "12.00" } ] }),
        )
        .await;
    assert_eq!(invoice["body"]["lines"][1]["tax_code_id"], json!(f.vat));
    assert_eq!(dec(&invoice["summary"]["total_net"]), 62.0);
    assert_eq!(dec(&invoice["summary"]["total_tax"]), 11.78);
    assert_eq!(dec(&invoice["summary"]["total_charges"]), 12.0);
    assert_eq!(dec(&invoice["summary"]["total_discount"]), 0.0);
    let books = f.books(&t).await;
    assert!(close(books["4000"], -50.0));
    assert!(close(books["4100"], -12.0));
    assert!(close(books["2300"], -11.78));
    assert!(close(books["1200"], 73.78));
    // The order is complete: the freight is not on it.
    assert_eq!(f.now(&t, "sales_order", &order).await["status"], "closed");

    // The freight is refunded: a credit for it alone needs no return.
    let freight_line = invoice["body"]["lines"][1]["id"].clone();
    let credit = f
        .posted(
            &t,
            "sales_credit_note",
            "2026-03-14",
            json!({ "sales_invoice_id": invoice["id"], "lines": [{ "invoice_line_id": freight_line, "qty": "1" }] }),
        )
        .await;
    assert_eq!(dec(&credit["summary"]["total_gross"]), 14.28);
    assert_eq!(dec(&credit["summary"]["total_charges"]), 12.0);
    let books = f.books(&t).await;
    assert!(close(books["4100"], 0.0));
    assert!(close(books["2300"], -9.5));

    // Reversed in turn, the credit puts the freight back.
    let (status, body) = t
        .command(
            &t.admin,
            "sales_credit_note.reverse",
            json!({ "id": credit["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(close(f.books(&t).await["4100"], -12.0));
    t.stop().await;
}

#[tokio::test]
async fn a_service_on_a_sales_order_is_invoiced_without_a_delivery() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // Installation, billed to an account of its own; and a line on the
    // receivables account is refused.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": f.c.id, "doc_date": "2026-03-01", "body": {
                "customer_id": f.customer, "lines": [
                    { "account_id": f.account["1200"], "description": "Installation",
                      "qty": "1", "unit_price": "80.00" } ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "account_id", "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("receivables account"),
        "{body}"
    );
    let order = f
        .approved(
            &t,
            "sales_order",
            json!({ "customer_id": f.customer, "lines": [
                { "account_id": f.account["4200"], "description": "Installation",
                  "qty": "2", "unit_price": "80.00", "discount_percent": "25" } ] }),
        )
        .await;
    assert_eq!(dec(&order["summary"]["total_net"]), 120.0);
    let line = order["body"]["lines"][0]["id"].clone();
    // Nothing is delivered of it.
    let draft = f
        .draft(
            &t,
            "sales_delivery",
            "2026-03-05",
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "2" }] }),
        )
        .await;
    let (status, body) = f.post(&t, "sales_delivery", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Invoiced from the order straight away; it completes the order.
    let (status, draft) = t
        .command(
            &t.admin,
            "sales_order.invoice",
            json!({ "id": order["id"], "doc_date": "2026-03-06" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let (status, invoice) = f.post(&t, "sales_invoice", &draft).await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    assert_eq!(dec(&invoice["summary"]["total_net"]), 120.0);
    let books = f.books(&t).await;
    assert!(close(books["4200"], -120.0));
    assert!(close(books["1200"], 120.0));
    assert_eq!(f.now(&t, "sales_order", &order).await["status"], "closed");
    t.stop().await;
}

impl Fixture {
    /// The inventory account holds what the stock ledger says the stock is
    /// worth.
    async fn stock_agrees(&self, t: &TestApp) -> f64 {
        let books = self.books(t).await;
        let (_, on_hand) = t
            .command(
                &t.admin,
                "stock.on_hand",
                json!({ "company_id": self.c.id }),
            )
            .await;
        let value = dec(&on_hand["total_value"]);
        assert!(
            close(value, books.get("1300").copied().unwrap_or(0.0)),
            "stock {value} and the inventory account {:?} agree",
            books.get("1300")
        );
        value
    }
}

#[tokio::test]
async fn a_vendor_invoice_without_an_order_books_rent_and_services() {
    let t = TestApp::start().await;
    let f = setup(&t).await;

    // Rent for March: 1,000.00 and 19 % tax, on the rent account.
    let rent = json!({ "vendor_id": f.vendor, "vendor_reference": "R-03", "lines": [
        { "account_id": f.account["6100"], "description": "Rent March", "qty": "1",
          "unit_price": "1000.00", "tax_code_id": f.vat } ] });
    let invoice = f.posted(&t, "vendor_invoice", "2026-03-01", rent).await;
    assert_eq!(invoice["summary"]["vendor_id"], json!(f.vendor));
    assert_eq!(dec(&invoice["summary"]["total_gross"]), 1190.0);
    let books = f.books(&t).await;
    assert!(close(books["6100"], 1000.0));
    assert!(close(books["1400"], 190.0));
    assert!(close(books["2000"], -1190.0));

    // Not on a control account, and no order lines without an order.
    for (line, field) in [
        (
            json!({ "account_id": f.account["2000"], "qty": "1", "unit_price": "5" }),
            "account_id",
        ),
        (
            json!({ "account_id": f.account["1400"], "qty": "1", "unit_price": "5" }),
            "account_id",
        ),
    ] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/documents/vendor_invoice",
                json!({ "company_id": f.c.id, "doc_date": "2026-03-02",
                        "body": { "vendor_id": f.vendor, "lines": [line] } }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["field"], field, "{body}");
    }

    // Half the rent is credited back: to the rent account, with its tax.
    let line = invoice["body"]["lines"][0]["id"].clone();
    let credit = f
        .posted(
            &t,
            "vendor_credit_note",
            "2026-03-05",
            json!({ "vendor_invoice_id": invoice["id"],
                    "lines": [{ "invoice_line_id": line, "qty": "1", "unit_price": "500.00" }] }),
        )
        .await;
    assert_eq!(dec(&credit["summary"]["total_gross"]), 595.0);
    let books = f.books(&t).await;
    assert!(close(books["6100"], 500.0));
    assert!(close(books["2000"], -595.0));

    // A company that wants such invoices checked refuses them above 5,000.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "company_id": f.c.id, "point": "vendor_invoice.without_order",
                    "effect": "refuse", "condition": "facts.total > 5000", "priority": 10 }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let draft = f
        .draft(
            &t,
            "vendor_invoice",
            "2026-03-06",
            json!({ "vendor_id": f.vendor, "lines": [
                { "account_id": f.account["6100"], "qty": "1", "unit_price": "6000.00" } ] }),
        )
        .await;
    let (status, body) = f.post(&t, "vendor_invoice", &draft).await;
    assert_ne!(status, StatusCode::OK, "{body}");

    // Reversed, credit note first, the rent is gone from the books.
    for (doc_type, doc) in [
        ("vendor_credit_note", &credit),
        ("vendor_invoice", &invoice),
    ] {
        let (status, body) = t
            .command(
                &t.admin,
                &format!("{doc_type}.reverse"),
                json!({ "id": doc["id"] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let books = f.books(&t).await;
    assert!(close(books["6100"], 0.0));
    assert!(close(books["2000"], 0.0));

    // A service on a purchase order is invoiced without a receipt, which
    // completes the order; and a direct sales invoice bills a service.
    let order = f
        .approved(
            &t,
            "purchase_order",
            json!({ "vendor_id": f.vendor, "lines": [
                { "account_id": f.account["6100"], "description": "Cleaning",
                  "qty": "1", "unit_price": "300.00" } ] }),
        )
        .await;
    let (status, draft) = t
        .command(
            &t.admin,
            "purchase_order.invoice",
            json!({ "id": order["id"], "doc_date": "2026-03-10" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let (status, body) = f.post(&t, "vendor_invoice", &draft).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        f.now(&t, "purchase_order", &order).await["status"],
        "closed"
    );
    let service = f
        .posted(
            &t,
            "sales_invoice",
            "2026-03-11",
            json!({ "customer_id": f.customer, "lines": [
                { "account_id": f.account["4200"], "description": "Consulting",
                  "qty": "2", "unit_price": "50.00", "tax_code_id": f.vat } ] }),
        )
        .await;
    assert_eq!(service["summary"]["customer_id"], json!(f.customer));
    let books = f.books(&t).await;
    assert!(close(books["6100"], 300.0));
    assert!(close(books["4200"], -100.0));
    assert!(close(books["1200"], 119.0));
    t.stop().await;
}

#[tokio::test]
async fn landed_cost_is_allocated_to_goods_received_and_keeps_inventory_equal_to_stock_value() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let gadget = t
        .create("item", json!({ "code": "GADGET", "name": "Gadget" }))
        .await;
    let freight = t
        .create(
            "charge",
            json!({ "code": "INBOUND", "name": "Inbound freight", "kind": "freight",
                    "landed_cost": true }),
        )
        .await;
    let carrier = t
        .create("vendor", json!({ "code": "CARRIER", "name": "Trucks Inc" }))
        .await;

    // 10 widgets at 10.00 (100.00) and 20 gadgets at 20.00 (400.00), each
    // on a receipt of its own.
    let order = f
        .approved(
            &t,
            "purchase_order",
            json!({ "vendor_id": f.vendor, "lines": [
                { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "10", "unit_price": "10.00" },
                { "item_id": gadget, "warehouse_id": f.warehouse, "qty": "20", "unit_price": "20.00" } ] }),
        )
        .await;
    let mut receipts = vec![];
    for (index, qty) in [(0, "10"), (1, "20")] {
        let receipt = f
            .posted(
                &t,
                "purchase_receipt",
                "2026-03-02",
                json!({ "purchase_order_id": order["id"], "lines": [
                    { "order_line_id": order["body"]["lines"][index]["id"], "qty": qty } ] }),
            )
            .await;
        receipts.push(receipt["id"].clone());
    }
    // Five widgets are sold.
    let sale = f
        .approved(
            &t,
            "sales_order",
            json!({ "customer_id": f.customer, "lines": [
                { "item_id": f.item, "warehouse_id": f.warehouse, "qty": "5", "unit_price": "30.00" } ] }),
        )
        .await;
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-03",
        json!({ "sales_order_id": sale["id"],
                "lines": [{ "order_line_id": sale["body"]["lines"][0]["id"], "qty": "5" }] }),
    )
    .await;
    assert_eq!(f.stock_agrees(&t).await, 450.0);

    // The carrier's invoice of 50.00, by value over both receipts: 10.00
    // for the widgets, 40.00 for the gadgets. Half the widgets are gone:
    // 5.00 of theirs is a cost of the goods sold, through price variance.
    let by_value = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-05",
            json!({ "vendor_id": carrier, "lines": [
                { "charge_id": freight, "qty": "1", "unit_price": "50.00",
                  "receipt_ids": receipts.clone() } ] }),
        )
        .await;
    assert_eq!(f.stock_agrees(&t).await, 495.0);
    let books = f.books(&t).await;
    assert!(close(books["5200"], 5.0), "{books:?}");
    assert!(close(books["2000"], -50.0));

    // 30.00 by quantity: 10.00 for the ten widgets, 20.00 for the twenty
    // gadgets; again half the widgets' share is for goods gone.
    let by_quantity = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-06",
            json!({ "vendor_id": carrier, "lines": [
                { "charge_id": freight, "qty": "1", "unit_price": "30.00",
                  "allocate_by": "quantity", "receipt_ids": receipts.clone() } ] }),
        )
        .await;
    assert_eq!(f.stock_agrees(&t).await, 520.0);
    assert!(close(f.books(&t).await["5200"], 10.0));

    // By weight: refused while the items have none; with a widget at 1.0
    // and a gadget at 0.5, 30.00 is 15.00 for each (ten widgets, twenty
    // gadgets), half the widgets' share again for goods gone.
    let by_weight = json!({ "vendor_id": carrier, "lines": [
        { "charge_id": freight, "qty": "1", "unit_price": "30.00",
          "allocate_by": "weight", "receipt_ids": receipts.clone() } ] });
    let draft = f
        .draft(&t, "vendor_invoice", "2026-03-07", by_weight.clone())
        .await;
    let (status, body) = f.post(&t, "vendor_invoice", &draft).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has no weight"),
        "{body}"
    );
    for (item, weight) in [(&f.item, "1.0"), (&gadget, "0.5")] {
        let (status, body) = t
            .patch(
                &t.admin,
                &format!("/api/entities/item/{item}"),
                json!({ "weight": weight }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, by_weight) = f.post(&t, "vendor_invoice", &draft).await;
    assert_eq!(status, StatusCode::OK, "{by_weight}");
    assert_eq!(f.stock_agrees(&t).await, 542.5);
    assert!(close(f.books(&t).await["5200"], 17.5));
    let (status, body) = t
        .command(
            &t.admin,
            "vendor_invoice.reverse",
            json!({ "id": by_weight["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Each reversed takes its share back out of stock and variance.
    for invoice in [&by_quantity, &by_value] {
        let (status, body) = t
            .command(
                &t.admin,
                "vendor_invoice.reverse",
                json!({ "id": invoice["id"] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(f.stock_agrees(&t).await, 450.0);
    let books = f.books(&t).await;
    assert!(close(books["5200"], 0.0));
    assert!(close(books["2000"], 0.0));
    t.stop().await;
}

#[tokio::test]
async fn a_machine_bought_on_a_vendor_invoice_is_capitalised_from_its_line() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let cost = t
        .create(
            "account",
            json!({ "company_id": f.c.id, "code": "1500", "name": "Machines", "account_type": "asset" }),
        )
        .await;
    let class = t
        .create(
            "asset_class",
            json!({ "company_id": f.c.id, "code": "MACH", "name": "Machinery",
                    "method": "straight_line", "useful_life_months": 60 }),
        )
        .await;
    t.create(
        "account_rule",
        json!({ "company_id": f.c.id, "purpose": "asset_cost", "scope_type": "asset_class",
                "scope_id": class, "account_id": cost }),
    )
    .await;
    // The invoice for the machine: 12,000.00 on the class's cost account,
    // with tax, owed to the vendor.
    let invoice = f
        .posted(
            &t,
            "vendor_invoice",
            "2026-03-15",
            json!({ "vendor_id": f.vendor, "lines": [
                { "account_id": cost, "description": "Lathe", "qty": "1",
                  "unit_price": "12000.00", "tax_code_id": f.vat } ] }),
        )
        .await;
    let books = f.books(&t).await;
    assert!(close(books["1500"], 12000.0));
    assert!(close(books["2000"], -14280.0));
    let asset = t
        .create(
            "asset",
            json!({ "company_id": f.c.id, "class_id": class, "code": "LATHE", "name": "Lathe" }),
        )
        .await;
    let (status, capitalised) = t
        .command(
            &t.admin,
            "asset.capitalise",
            json!({ "id": asset, "document_id": invoice["id"],
                    "line_id": invoice["body"]["lines"][0]["id"],
                    "in_service_date": "2026-04-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{capitalised}");
    assert_eq!(dec(&capitalised["cost"]), 12000.0);
    assert_eq!(capitalised["status"], "in_service");
    t.stop().await;
}
