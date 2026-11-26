//! Review 9: German printed invoices. Each test fails on the code as it is.

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

/// A US company, as currency.rs sets it up, that writes German.
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
    for (code, kind) in [
        ("1200", "asset"),
        ("1300", "asset"),
        ("1400", "asset"),
        ("2000", "liability"),
        ("2100", "liability"),
        ("2300", "liability"),
        ("5000", "expense"),
        ("5200", "expense"),
        ("7100", "income"),
        ("7200", "expense"),
    ] {
        let data = json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind });
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
    for (currency, date, rate) in [("CAD", "2026-03-01", "0.75")] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": currency, "to_currency": "USD", "rate_date": date, "rate": rate }),
        )
        .await;
    }
    let (status, body) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "company", "party_id": c.id, "profile": { "language": "de" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Fixture {
        c,
        item,
        warehouse,
        vendor,
        customer,
        vat,
    }
}

impl Fixture {
    async fn posted(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.c.id, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
        let (status, done) = t
            .command(
                &t.admin,
                &format!("{doc_type}.post"),
                json!({ "id": id(&draft) }),
            )
            .await;
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
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{kind}_order"),
                json!({ "company_id": self.c.id, "doc_date": "2026-03-02", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
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

    async fn stock(&self, t: &TestApp) {
        let buy = self.order(t, "purchase", "USD", "10", "2.00").await;
        self.posted(
            t,
            "purchase_receipt",
            "2026-03-03",
            json!({ "purchase_order_id": buy["id"], "lines": [{ "order_line_id": buy["body"]["lines"][0]["id"], "qty": "10" }] }),
        )
        .await;
    }
}

async fn page(t: &TestApp, id: &str) -> String {
    let (status, rendered) = t
        .command(
            &t.admin,
            "document.render",
            json!({ "doc_type": "sales_invoice", "id": id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{rendered}");
    rendered["html"].as_str().unwrap().to_owned()
}

/// A German invoice in Canadian dollars of a US company: 4 at 9.00 CAD is
/// 36.00 CAD net, 6.84 CAD tax, 42.84 CAD gross. The tax line by rate
/// must say 6,84 on 36,00, in the invoice's currency beside its total.
/// It takes the tax ledger's base and amount, which are in the company's
/// accounting currency (27.00 and 5.13 USD), and prints them unlabelled
/// between a net and a gross in CAD: net + tax shown != gross shown.
#[tokio::test]
async fn a_german_invoice_in_a_foreign_currency_states_its_tax_in_that_currency() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock(&t).await;
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
    let html = page(&t, &id(&invoice)).await;
    t.stop().await;
    assert!(html.contains("42,84"), "{html}");
    assert!(
        html.contains("Umsatzsteuer 19 % auf 36,00") && html.contains("6,84"),
        "the tax by rate is not in the invoice's currency (CAD 36,00 / 6,84):\n{}",
        html.lines()
            .filter(|l| l.contains("Umsatzsteuer") || l.contains("Gesamtbetrag"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // And, as § 16 Abs. 6 UStG asks, the tax in the accounting currency:
    // 5.13 USD on 27.00.
    assert!(
        html.contains("§ 16 Abs. 6") && html.contains("auf 27,00 USD: 5,13 USD"),
        "{html}"
    );
}

/// The Leistungsdatum of the second of two partial invoices: delivered 2
/// on March 5 (invoiced March 6), then 2 on March 10, invoiced March 15.
/// The second invoice is for the second delivery only, so its date of
/// supply is 10.03.2026; it prints "05.03.2026 – 10.03.2026", the dates
/// of every delivery of the order, including one billed by another invoice.
#[tokio::test]
async fn the_date_of_supply_is_that_of_the_deliveries_the_invoice_bills() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock(&t).await;
    let order = f.order(&t, "sales", "USD", "4", "10.00").await;
    let line = order["body"]["lines"][0]["id"].clone();
    for (delivered, invoiced) in [("2026-03-05", "2026-03-06"), ("2026-03-10", "2026-03-15")] {
        f.posted(
            &t,
            "sales_delivery",
            delivered,
            json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "2" }] }),
        )
        .await;
        let invoice = f
            .posted(
                &t,
                "sales_invoice",
                invoiced,
                json!({ "sales_order_id": order["id"],
                        "lines": [{ "order_line_id": line, "qty": "2", "unit_price": "10.00" }] }),
            )
            .await;
        if invoiced == "2026-03-15" {
            let html = page(&t, &id(&invoice)).await;
            let shown: Vec<&str> = html
                .lines()
                .filter(|l| l.contains("Leistungsdatum"))
                .collect();
            t.stop().await;
            assert!(
                !html.contains("05.03.2026"),
                "the second invoice states the first delivery's date as its date of supply: {shown:?}"
            );
            return;
        }
    }
    unreachable!();
}

/// What § 14 Abs. 4 Nr. 9 UStG asks of an exempt invoice, the reason for
/// the exemption, is printed only for someone who may read the tax
/// ledger: the same posted invoice printed by a clerk who may read sales
/// invoices but not tax entries loses the note (and the tax by rate). The
/// legal content of a document must not depend on who prints it.
#[tokio::test]
async fn the_exemption_note_does_not_depend_on_who_prints() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    f.stock(&t).await;
    let exempt = t
        .create(
            "tax_code",
            json!({ "code": "DE-0", "name": "steuerfrei", "rate": "0", "vat_category": "E",
                    "exemption_reason": "Steuerfreie innergemeinschaftliche Lieferung" }),
        )
        .await;
    let settings = t
        .get(
            &t.admin,
            &format!("/api/entities/tax_settings?company_id={}", f.c.id),
        )
        .await
        .1["items"][0]
        .clone();
    t.create(
        "tax_settings",
        json!({ "company_id": f.c.id, "tax_code_id": exempt,
                "collected_account_id": settings["collected_account_id"],
                "paid_account_id": settings["paid_account_id"] }),
    )
    .await;
    let order = f.order(&t, "sales", "USD", "1", "10.00").await;
    let line = order["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-03-05",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "1" }] }),
    )
    .await;
    let invoice = f
        .posted(
            &t,
            "sales_invoice",
            "2026-03-06",
            json!({ "sales_order_id": order["id"],
                    "lines": [{ "order_line_id": line, "qty": "1", "unit_price": "10.00", "tax_code_id": exempt }] }),
        )
        .await;
    let admin = page(&t, &id(&invoice)).await;
    let clerk = t
        .user_with_role("clerk@example.com", &["sales_invoice:read"], &f.c.id)
        .await;
    let (status, rendered) = t
        .command(
            &clerk,
            "document.render",
            json!({ "doc_type": "sales_invoice", "id": id(&invoice) }),
        )
        .await;
    t.stop().await;
    assert!(
        admin.contains("Steuerfreie innergemeinschaftliche Lieferung"),
        "not even the admin's print has the note: {admin}"
    );
    assert_eq!(status, StatusCode::OK, "{rendered}");
    let html = rendered["html"].as_str().unwrap_or_default();
    assert!(
        html.contains("Steuerfreie innergemeinschaftliche Lieferung"),
        "the clerk's print of the same invoice has no exemption note:\n{}",
        html.lines()
            .filter(|l| l.contains("Umsatzsteuer") || l.contains("<p>"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The English invoice (and order) of the same Canadian-dollar sale labels
/// its grand total with the company's currency: "Total USD 42.84" for an
/// amount in CAD. The German template says the document's currency; the
/// English one does not.
#[tokio::test]
async fn an_english_invoice_in_a_foreign_currency_names_that_currency_on_its_total() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let (status, body) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer", "party_id": f.customer, "profile": { "language": "en" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    f.stock(&t).await;
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
    let html = page(&t, &id(&invoice)).await;
    t.stop().await;
    let total: Vec<&str> = html.lines().filter(|l| l.contains("grand")).collect();
    assert!(html.contains("42.84"), "{html}");
    assert!(
        total.iter().any(|l| l.contains("CAD")),
        "the total of a CAD invoice is labelled with another currency: {total:?}"
    );
}
