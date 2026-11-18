//! Electronic invoices: a German company's sales invoices and credit notes
//! written as XRechnung (UBL and CII) and checked; incoming XRechnung in
//! UBL and CII, and a ZUGFeRD PDF, drafted as vendor invoices; hostile and
//! broken files refused.

mod common;

use axum::http::StatusCode;
use base64::Engine;
use common::{TestApp, dec, id};
use erpcraft_einvoice::model::{Invoice, check};
use serde_json::{Value, json};

struct Fixture {
    company: String,
    account: std::collections::BTreeMap<&'static str, String>,
    vat19: String,
    vat7: String,
    widget: String,
    book: String,
    warehouse: String,
    customer: String,
    vendor: String,
    freight: String,
    /// The purchase order the stock came in with.
    order: Value,
}

async fn scalar(t: &TestApp, sql: &str) -> String {
    sqlx::query_scalar(sql).fetch_one(&t.pool).await.unwrap()
}

async fn setup(t: &TestApp) -> Fixture {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "de_skr03", "code": "DE", "name": "Muster GmbH", "first_year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let company = body["company"]["id"].as_str().unwrap().to_owned();
    let mut account = std::collections::BTreeMap::new();
    for code in ["1200", "8400", "4930"] {
        account.insert(
            code,
            scalar(
                t,
                &format!(
                    "SELECT id::text FROM account WHERE company_id = '{company}' AND code = '{code}'"
                ),
            )
            .await,
        );
    }
    let vat19 = scalar(t, "SELECT id::text FROM tax_code WHERE code = 'DE-USt19'").await;
    let vat7 = scalar(t, "SELECT id::text FROM tax_code WHERE code = 'DE-USt7'").await;
    let warehouse = scalar(
        t,
        &format!("SELECT id::text FROM warehouse WHERE company_id = '{company}'"),
    )
    .await;
    let widget = t
        .create(
            "item",
            json!({ "code": "WIDGET", "name": "Widget", "uom": "pcs" }),
        )
        .await;
    let book = t
        .create(
            "item",
            json!({ "code": "BOOK", "name": "Handbuch", "uom": "pcs" }),
        )
        .await;
    let customer = t
        .create("customer", json!({ "code": "C1", "name": "Kunde AG" }))
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V1", "name": "Lieferant GmbH" }))
        .await;
    let freight = t
        .create(
            "charge",
            json!({ "code": "FRACHT", "name": "Fracht", "kind": "freight", "tax_code_id": vat19 }),
        )
        .await;
    t.create(
        "account_rule",
        json!({ "company_id": company, "purpose": "charge_revenue", "account_id": account["8400"] }),
    )
    .await;

    // Stock to sell, bought on an order that stays open for an invoice.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/purchase_order",
            json!({ "company_id": company, "doc_date": "2026-03-01", "body": {
                "vendor_id": vendor,
                "lines": [
                    { "item_id": widget, "warehouse_id": warehouse, "qty": "100", "unit_price": "20.00" },
                    { "item_id": book, "warehouse_id": warehouse, "qty": "50", "unit_price": "5.00" },
                ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, order) = t
        .command(
            &t.admin,
            "purchase_order.approve",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    let lines = order["body"]["lines"].as_array().unwrap();
    let (status, receipt) = t
        .post(
            &t.admin,
            "/api/documents/purchase_receipt",
            json!({ "company_id": company, "doc_date": "2026-03-02", "body": {
                "purchase_order_id": order["id"],
                "lines": [
                    { "order_line_id": lines[0]["id"], "qty": "100" },
                    { "order_line_id": lines[1]["id"], "qty": "50" },
                ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    let (status, body) = t
        .command(
            &t.admin,
            "purchase_receipt.post",
            json!({ "id": receipt["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // What an e-invoice needs that the books do not keep: the parties'
    // particulars, addresses, tax identifiers, contacts and electronic
    // addresses, kept by the party module.
    party(
        t,
        "company",
        &company,
        json!({
            "profile": { "legal_name": "Muster GmbH",
                         "legal_registration": "HRB 12345, Amtsgericht München",
                         "payment_account_id": account["1200"] },
            "address": { "street": "Hauptstraße 1", "city": "München", "postcode": "80331",
                         "country_code": "DE" },
            "tax_ids": [["eu_vat", "DE123456789"], ["tax_number", "143/123/45678"]],
            "contact": { "name": "Buchhaltung", "phone": "+49 89 1234567",
                         "email": "rechnung@muster.example" },
            "electronic_address": "rechnung@muster.example",
        }),
    )
    .await;
    // The account paid into, known by its numbers in banking.
    t.create(
        "bank_account_number",
        json!({ "company_id": company, "bank_account_id": account["1200"],
                "iban": "DE89370400440532013000", "bic": "COBADEFFXXX" }),
    )
    .await;
    party(
        t,
        "customer",
        &customer,
        json!({
            "profile": { "invoice_reference": "04011000-12345-03" },
            "address": { "street": "Marktplatz 5", "city": "Köln", "postcode": "50667",
                         "country_code": "DE" },
            "tax_ids": [["eu_vat", "DE987654321"]],
            "electronic_address": "einkauf@kunde.example",
        }),
    )
    .await;
    party(
        t,
        "vendor",
        &vendor,
        json!({ "tax_ids": [["eu_vat", "DE 111 111 111"]] }),
    )
    .await;
    Fixture {
        company,
        account,
        vat19,
        vat7,
        widget,
        book,
        warehouse,
        customer,
        vendor,
        freight,
        order,
    }
}

/// Record a party's data with `party.set`: `profile`, a default billing
/// `address`, `tax_ids` as [scheme, value] pairs, a default `contact` and a
/// default `electronic_address` (email).
async fn party(t: &TestApp, kind: &str, id: &str, data: Value) {
    let mut input = json!({ "party_type": kind, "party_id": id });
    if let Some(profile) = data.get("profile") {
        input["profile"] = profile.clone();
    }
    if let Some(address) = data.get("address") {
        let mut address = address.clone();
        address["is_default"] = json!(true);
        input["addresses"] = json!([address]);
    }
    if let Some(pairs) = data["tax_ids"].as_array() {
        input["tax_ids"] = pairs
            .iter()
            .map(|p| json!({ "scheme": p[0], "value": p[1] }))
            .collect();
    }
    if let Some(contact) = data.get("contact") {
        let mut contact = contact.clone();
        contact["is_default"] = json!(true);
        input["contacts"] = json!([contact]);
    }
    if let Some(address) = data.get("electronic_address") {
        input["electronic_addresses"] = json!([{ "value": address, "is_default": true }]);
    }
    let (status, body) = t.command(&t.admin, "party.set", input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

impl Fixture {
    async fn posted(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.company, "doc_date": date, "body": body }),
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

    /// A posted invoice to `customer`: 10 widgets at 25.00 less 10 % (19 %),
    /// 4 handbooks at 12.50 less 2.00 (7 %), and 9.90 freight (19 %).
    async fn invoice(&self, t: &TestApp, customer: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/sales_order",
                json!({ "company_id": self.company, "doc_date": "2026-04-01", "body": {
                    "customer_id": customer,
                    "lines": [
                        { "item_id": self.widget, "warehouse_id": self.warehouse, "qty": "10",
                          "unit_price": "25.00", "discount_percent": "10" },
                        { "item_id": self.book, "warehouse_id": self.warehouse, "qty": "4",
                          "unit_price": "12.50", "discount_amount": "2.00" },
                    ] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, order) = t
            .command(
                &t.admin,
                "sales_order.approve",
                json!({ "id": draft["id"] }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{order}");
        let lines = order["body"]["lines"].as_array().unwrap().clone();
        self.posted(
            t,
            "sales_delivery",
            "2026-04-02",
            json!({ "sales_order_id": order["id"], "lines": [
                { "order_line_id": lines[0]["id"], "qty": "10" },
                { "order_line_id": lines[1]["id"], "qty": "4" } ] }),
        )
        .await;
        self.posted(
            t,
            "sales_invoice",
            "2026-04-03",
            json!({ "sales_order_id": order["id"], "lines": [
                { "order_line_id": lines[0]["id"], "qty": "10", "unit_price": "25.00",
                  "tax_code_id": self.vat19 },
                { "order_line_id": lines[1]["id"], "qty": "4", "unit_price": "12.50",
                  "tax_code_id": self.vat7 },
                { "charge_id": self.freight, "qty": "1", "unit_price": "9.90" } ] }),
        )
        .await
    }
}

async fn export(t: &TestApp, doc_type: &str, doc: &Value, syntax: &str) -> (StatusCode, Value) {
    t.command(
        &t.admin,
        "einvoice.export",
        json!({ "doc_type": doc_type, "id": doc["id"], "syntax": syntax }),
    )
    .await
}

fn read(xml: &str) -> Invoice {
    erpcraft_einvoice::read_xml(xml).unwrap()
}

fn amount(v: erpcraft_kernel::Decimal) -> f64 {
    v.to_string().parse().unwrap()
}

#[tokio::test]
async fn a_german_invoice_is_written_as_xrechnung() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let invoice = f.invoice(&t, &f.customer).await;
    // 250.00 less 25.00 is 225.00, tax 42.75; 50.00 less 2.00 is 48.00,
    // tax 3.36; freight 9.90, tax 1.88.
    assert_eq!(dec(&invoice["summary"]["total_net"]), 282.9);
    assert_eq!(dec(&invoice["summary"]["total_tax"]), 47.99);

    for syntax in ["ubl", "cii"] {
        let (status, out) = export(&t, "sales_invoice", &invoice, syntax).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["valid"], true);
        assert_eq!(out["syntax"], syntax);
        assert!(
            out["filename"]
                .as_str()
                .unwrap()
                .ends_with(&format!("-{syntax}.xml"))
        );
        let xml = out["xml"].as_str().unwrap();
        assert!(xml.starts_with("<?xml"));
        assert!(
            xml.contains("urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0")
        );
        assert!(xml.contains("04011000-12345-03"));
        assert!(xml.contains("DE89370400440532013000"));
        assert!(xml.contains("#SKONTO#") || !xml.contains("SKONTO"));
        if syntax == "ubl" {
            assert!(xml.contains("<ubl:Invoice"));
            assert!(xml.contains("<cbc:InvoiceTypeCode>380</cbc:InvoiceTypeCode>"));
            assert!(
                xml.contains("<cbc:PayableAmount currencyID=\"EUR\">330.89</cbc:PayableAmount>")
            );
            assert!(xml.contains("<cbc:MultiplierFactorNumeric>10</cbc:MultiplierFactorNumeric>"));
            assert!(xml.contains("<cbc:BaseAmount currencyID=\"EUR\">250.00</cbc:BaseAmount>"));
            assert!(xml.contains("<cbc:PaymentMeansCode>58</cbc:PaymentMeansCode>"));
        } else {
            assert!(xml.contains("<rsm:CrossIndustryInvoice"));
            assert!(xml.contains("<ram:DuePayableAmount>330.89</ram:DuePayableAmount>"));
            assert!(
                xml.contains("<udt:DateTimeString format=\"102\">20260403</udt:DateTimeString>")
            );
        }

        // Read back: every rule checked here holds, and the figures are
        // the books'.
        let parsed = read(xml);
        let fatal: Vec<_> = check(&parsed).into_iter().filter(|f| f.fatal).collect();
        assert!(fatal.is_empty(), "{fatal:?}");
        assert_eq!(parsed.number, invoice["number"].as_str().unwrap());
        assert_eq!(parsed.issue_date.to_string(), "2026-04-03");
        assert_eq!(parsed.delivery_date.unwrap().to_string(), "2026-04-02");
        assert_eq!(parsed.seller.vat_id.as_deref(), Some("DE123456789"));
        assert_eq!(parsed.seller.tax_number.as_deref(), Some("143/123/45678"));
        assert_eq!(parsed.buyer.city.as_deref(), Some("Köln"));
        assert_eq!(parsed.buyer.identifier.as_deref(), Some("C1"));
        assert_eq!(parsed.lines.len(), 3);
        let widget = &parsed.lines[0];
        assert_eq!(widget.name, "Widget");
        assert_eq!(widget.unit, "H87");
        assert_eq!(widget.seller_item_id.as_deref(), Some("WIDGET"));
        assert_eq!(amount(widget.net_amount), 225.0);
        assert_eq!(amount(widget.allowances[0].amount), 25.0);
        assert_eq!(amount(parsed.lines[1].allowances[0].amount), 2.0);
        assert_eq!(parsed.lines[2].name, "Fracht");
        assert_eq!(parsed.lines[2].unit, "C62");
        assert_eq!(parsed.vat.len(), 2);
        let rate = |r: &str| {
            parsed
                .vat
                .iter()
                .find(|v| v.rate.unwrap().to_string() == r)
                .unwrap()
                .clone()
        };
        assert_eq!(rate("19").category, "S");
        assert_eq!(amount(rate("19").taxable), 234.9);
        assert_eq!(amount(rate("19").tax), 44.63);
        assert_eq!(amount(rate("7").taxable), 48.0);
        assert_eq!(amount(rate("7").tax), 3.36);
        assert_eq!(amount(parsed.totals.line_total), 282.9);
        assert_eq!(amount(parsed.totals.tax_total), 47.99);
        assert_eq!(amount(parsed.totals.payable), 330.89);
        assert_eq!(
            parsed.payment.as_ref().unwrap().bic.as_deref(),
            Some("COBADEFFXXX")
        );
    }

    // The plain EN 16931 profile names the standard alone.
    let (status, out) = t
        .command(
            &t.admin,
            "einvoice.export",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"], "profile": "en16931" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["specification"], "urn:cen.eu:en16931:2017");

    // The addresses are the documents' too: the parties of the invoice,
    // and the printed invoice shows both.
    let (status, parties) = t
        .command(
            &t.admin,
            "document.parties",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{parties}");
    assert_eq!(parties["seller"]["kind"], "company");
    assert_eq!(parties["seller"]["name"], "Muster GmbH");
    assert_eq!(parties["buyer"]["city"], "Köln");
    let (status, page) = t
        .command(
            &t.admin,
            "document.render",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let html = page["html"].as_str().unwrap();
    for shown in [
        "Hauptstraße 1",
        "80331 München",
        "DE123456789",
        "Marktplatz 5",
        "50667 Köln",
        "DE987654321",
        "04011000-12345-03",
    ] {
        assert!(html.contains(shown), "{shown} is printed");
    }
    // Someone who may read the invoice but not the customer sees no
    // buyer's address.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["sales_invoice:read"]),
            Some(&f.company),
        )
        .await;
    let (status, parties) = t
        .command(
            &clerk,
            "document.parties",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{parties}");
    assert_eq!(parties["buyer"], Value::Null);
    assert_eq!(parties["seller"]["city"], Value::Null);
    t.stop().await;
}

#[tokio::test]
async fn a_credit_note_is_written_against_its_invoice() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let invoice = f.invoice(&t, &f.customer).await;
    let freight_line = invoice["body"]["lines"][2]["id"].clone();
    let credit = f
        .posted(
            &t,
            "sales_credit_note",
            "2026-04-10",
            json!({ "sales_invoice_id": invoice["id"],
                    "lines": [{ "invoice_line_id": freight_line, "qty": "1" }] }),
        )
        .await;
    for syntax in ["ubl", "cii"] {
        let (status, out) = export(&t, "sales_credit_note", &credit, syntax).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        let xml = out["xml"].as_str().unwrap();
        if syntax == "ubl" {
            assert!(xml.contains("<ubl:CreditNote"));
            assert!(xml.contains("<cbc:CreditNoteTypeCode>381</cbc:CreditNoteTypeCode>"));
            assert!(xml.contains("<cac:CreditNoteLine>"));
        }
        let parsed = read(xml);
        assert!(
            check(&parsed).iter().all(|f| !f.fatal),
            "{:?}",
            check(&parsed)
        );
        assert_eq!(parsed.type_code, "381");
        assert_eq!(
            parsed.preceding_invoice.as_deref(),
            invoice["number"].as_str()
        );
        assert_eq!(
            parsed.preceding_invoice_date.unwrap().to_string(),
            "2026-04-03"
        );
        assert_eq!(amount(parsed.totals.payable), 11.78);
        assert_eq!(parsed.payment.as_ref().unwrap().means_code, "97");
    }
    t.stop().await;
}

#[tokio::test]
async fn what_an_invoice_needs_is_named_with_where_to_set_it() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    // A customer the company has no e-invoice record for.
    let other = t
        .create("customer", json!({ "code": "C2", "name": "Neu KG" }))
        .await;
    let invoice = f.invoice(&t, &other).await;
    let (status, body) = export(&t, "sales_invoice", &invoice, "ubl").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("a billing address of customer C2 (party.set)"),
        "{message}"
    );

    // With an address but no buyer reference: XRechnung needs one; the
    // plain standard does not.
    party(
        &t,
        "customer",
        &other,
        json!({
            "address": { "city": "Bonn", "postcode": "53111", "country_code": "DE" },
            "electronic_address": "post@neu.example",
        }),
    )
    .await;
    let (status, body) = export(&t, "sales_invoice", &invoice, "ubl").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("buyer reference (BT-10)"), "{message}");
    assert!(
        message.contains("invoice_reference in the particulars of customer C2 (party.set)"),
        "{message}"
    );
    let (status, body) = t
        .command(
            &t.admin,
            "einvoice.export",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"], "profile": "en16931" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A tax code at 0 % must say which category it is.
    let zero = t
        .create(
            "tax_code",
            json!({ "code": "DE-0", "name": "Steuerfrei", "rate": "0" }),
        )
        .await;
    let paid = scalar(
        &t,
        &format!(
            "SELECT id::text FROM account WHERE company_id = '{}' AND code = '1576'",
            f.company
        ),
    )
    .await;
    let collected = scalar(
        &t,
        &format!(
            "SELECT id::text FROM account WHERE company_id = '{}' AND code = '1776'",
            f.company
        ),
    )
    .await;
    t.create(
        "tax_settings",
        json!({ "company_id": f.company, "tax_code_id": zero,
                "collected_account_id": collected, "paid_account_id": paid }),
    )
    .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": f.company, "doc_date": "2026-04-01", "body": {
                "customer_id": f.customer,
                "lines": [{ "item_id": f.widget, "warehouse_id": f.warehouse, "qty": "2",
                            "unit_price": "10.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (_, order) = t
        .command(
            &t.admin,
            "sales_order.approve",
            json!({ "id": draft["id"] }),
        )
        .await;
    let line = order["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-04-02",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "2" }] }),
    )
    .await;
    let exempt = f
        .posted(
            &t,
            "sales_invoice",
            "2026-04-03",
            json!({ "sales_order_id": order["id"], "lines": [
                { "order_line_id": line, "qty": "1", "unit_price": "10.00", "tax_code_id": zero }] }),
        )
        .await;
    let (status, body) = export(&t, "sales_invoice", &exempt, "ubl").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("einvoice_tax_category for DE-0"),
        "{body}"
    );
    t.create(
        "einvoice_tax_category",
        json!({ "tax_code_id": zero, "category": "E",
                "exemption_reason": "Steuerfrei nach § 4 Nr. 8 UStG" }),
    )
    .await;
    let (status, out) = export(&t, "sales_invoice", &exempt, "ubl").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(
        out["xml"]
            .as_str()
            .unwrap()
            .contains("<cbc:TaxExemptionReason>")
    );

    // A draft is not an invoice yet; an IBAN whose check digits are wrong
    // is not sent.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_invoice",
            json!({ "company_id": f.company, "doc_date": "2026-04-03",
                    "body": { "sales_order_id": order["id"], "lines": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, _) = export(&t, "sales_invoice", &draft, "ubl").await;
    assert_eq!(status, StatusCode::CONFLICT);
    sqlx::query("UPDATE bank_account_number SET iban = 'DE89370400440532013001'")
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, body) = export(&t, "sales_invoice", &exempt, "ubl").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("BR-DE-19"),
        "{body}"
    );
    t.stop().await;
}

/// An XRechnung in UBL from the vendor, for 5 widgets on our order and a
/// packaging line, at 19 %.
fn incoming_ubl(order_number: &str, number: &str, seller_vat: &str, seller: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ubl:Invoice xmlns:ubl="urn:oasis:names:specification:ubl:schema:xsd:Invoice-2"
  xmlns:cac="urn:oasis:names:specification:ubl:schema:xsd:CommonAggregateComponents-2"
  xmlns:cbc="urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2">
  <cbc:CustomizationID>urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0</cbc:CustomizationID>
  <cbc:ProfileID>urn:fdc:peppol.eu:2017:poacc:billing:01:1.0</cbc:ProfileID>
  <cbc:ID>{number}</cbc:ID>
  <cbc:IssueDate>2026-04-05</cbc:IssueDate>
  <cbc:DueDate>2026-05-05</cbc:DueDate>
  <cbc:InvoiceTypeCode>380</cbc:InvoiceTypeCode>
  <cbc:DocumentCurrencyCode>EUR</cbc:DocumentCurrencyCode>
  <cbc:BuyerReference>Muster-Einkauf</cbc:BuyerReference>
  <cac:OrderReference><cbc:ID>{order_number}</cbc:ID></cac:OrderReference>
  <cac:AccountingSupplierParty><cac:Party>
    <cbc:EndpointID schemeID="EM">rechnung@lieferant.example</cbc:EndpointID>
    <cac:PostalAddress><cbc:StreetName>Werkstraße 2</cbc:StreetName><cbc:CityName>Hamburg</cbc:CityName>
      <cbc:PostalZone>20095</cbc:PostalZone><cac:Country><cbc:IdentificationCode>DE</cbc:IdentificationCode></cac:Country></cac:PostalAddress>
    <cac:PartyTaxScheme><cbc:CompanyID>{seller_vat}</cbc:CompanyID><cac:TaxScheme><cbc:ID>VAT</cbc:ID></cac:TaxScheme></cac:PartyTaxScheme>
    <cac:PartyLegalEntity><cbc:RegistrationName>{seller}</cbc:RegistrationName></cac:PartyLegalEntity>
    <cac:Contact><cbc:Name>Vertrieb</cbc:Name><cbc:Telephone>+49 40 555123</cbc:Telephone><cbc:ElectronicMail>vertrieb@lieferant.example</cbc:ElectronicMail></cac:Contact>
  </cac:Party></cac:AccountingSupplierParty>
  <cac:AccountingCustomerParty><cac:Party>
    <cbc:EndpointID schemeID="EM">rechnung@muster.example</cbc:EndpointID>
    <cac:PostalAddress><cbc:CityName>München</cbc:CityName><cbc:PostalZone>80331</cbc:PostalZone>
      <cac:Country><cbc:IdentificationCode>DE</cbc:IdentificationCode></cac:Country></cac:PostalAddress>
    <cac:PartyTaxScheme><cbc:CompanyID>DE123456789</cbc:CompanyID><cac:TaxScheme><cbc:ID>VAT</cbc:ID></cac:TaxScheme></cac:PartyTaxScheme>
    <cac:PartyLegalEntity><cbc:RegistrationName>Muster GmbH</cbc:RegistrationName></cac:PartyLegalEntity>
  </cac:Party></cac:AccountingCustomerParty>
  <cac:Delivery><cbc:ActualDeliveryDate>2026-04-04</cbc:ActualDeliveryDate></cac:Delivery>
  <cac:PaymentMeans><cbc:PaymentMeansCode>58</cbc:PaymentMeansCode>
    <cac:PayeeFinancialAccount><cbc:ID>DE79000000001234567890</cbc:ID></cac:PayeeFinancialAccount></cac:PaymentMeans>
  <cac:PaymentTerms><cbc:Note>30 Tage netto</cbc:Note></cac:PaymentTerms>
  <cac:TaxTotal><cbc:TaxAmount currencyID="EUR">19.95</cbc:TaxAmount>
    <cac:TaxSubtotal><cbc:TaxableAmount currencyID="EUR">105.00</cbc:TaxableAmount><cbc:TaxAmount currencyID="EUR">19.95</cbc:TaxAmount>
      <cac:TaxCategory><cbc:ID>S</cbc:ID><cbc:Percent>19</cbc:Percent><cac:TaxScheme><cbc:ID>VAT</cbc:ID></cac:TaxScheme></cac:TaxCategory></cac:TaxSubtotal>
  </cac:TaxTotal>
  <cac:LegalMonetaryTotal>
    <cbc:LineExtensionAmount currencyID="EUR">105.00</cbc:LineExtensionAmount>
    <cbc:TaxExclusiveAmount currencyID="EUR">105.00</cbc:TaxExclusiveAmount>
    <cbc:TaxInclusiveAmount currencyID="EUR">124.95</cbc:TaxInclusiveAmount>
    <cbc:PayableAmount currencyID="EUR">124.95</cbc:PayableAmount>
  </cac:LegalMonetaryTotal>
  <cac:InvoiceLine><cbc:ID>1</cbc:ID><cbc:InvoicedQuantity unitCode="H87">5</cbc:InvoicedQuantity>
    <cbc:LineExtensionAmount currencyID="EUR">100.00</cbc:LineExtensionAmount>
    <cac:OrderLineReference><cbc:LineID>1</cbc:LineID></cac:OrderLineReference>
    <cac:Item><cbc:Name>Widget</cbc:Name><cac:BuyersItemIdentification><cbc:ID>WIDGET</cbc:ID></cac:BuyersItemIdentification>
      <cac:ClassifiedTaxCategory><cbc:ID>S</cbc:ID><cbc:Percent>19</cbc:Percent><cac:TaxScheme><cbc:ID>VAT</cbc:ID></cac:TaxScheme></cac:ClassifiedTaxCategory></cac:Item>
    <cac:Price><cbc:PriceAmount currencyID="EUR">20.00</cbc:PriceAmount></cac:Price></cac:InvoiceLine>
  <cac:InvoiceLine><cbc:ID>2</cbc:ID><cbc:InvoicedQuantity unitCode="C62">1</cbc:InvoicedQuantity>
    <cbc:LineExtensionAmount currencyID="EUR">5.00</cbc:LineExtensionAmount>
    <cac:Item><cbc:Name>Verpackung</cbc:Name>
      <cac:ClassifiedTaxCategory><cbc:ID>S</cbc:ID><cbc:Percent>19</cbc:Percent><cac:TaxScheme><cbc:ID>VAT</cbc:ID></cac:TaxScheme></cac:ClassifiedTaxCategory></cac:Item>
    <cac:Price><cbc:PriceAmount currencyID="EUR">5.00</cbc:PriceAmount></cac:Price></cac:InvoiceLine>
</ubl:Invoice>"#
    )
}

/// The same kind of invoice in CII, with no order reference: a direct
/// invoice for a service, with a line discount.
fn incoming_cii(number: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rsm:CrossIndustryInvoice xmlns:rsm="urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100"
  xmlns:ram="urn:un:unece:uncefact:data:standard:ReusableAggregateBusinessInformationEntity:100"
  xmlns:udt="urn:un:unece:uncefact:data:standard:UnqualifiedDataType:100">
  <rsm:ExchangedDocumentContext>
    <ram:BusinessProcessSpecifiedDocumentContextParameter><ram:ID>urn:fdc:peppol.eu:2017:poacc:billing:01:1.0</ram:ID></ram:BusinessProcessSpecifiedDocumentContextParameter>
    <ram:GuidelineSpecifiedDocumentContextParameter><ram:ID>urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0</ram:ID></ram:GuidelineSpecifiedDocumentContextParameter>
  </rsm:ExchangedDocumentContext>
  <rsm:ExchangedDocument><ram:ID>{number}</ram:ID><ram:TypeCode>380</ram:TypeCode>
    <ram:IssueDateTime><udt:DateTimeString format="102">20260406</udt:DateTimeString></ram:IssueDateTime></rsm:ExchangedDocument>
  <rsm:SupplyChainTradeTransaction>
    <ram:IncludedSupplyChainTradeLineItem>
      <ram:AssociatedDocumentLineDocument><ram:LineID>1</ram:LineID></ram:AssociatedDocumentLineDocument>
      <ram:SpecifiedTradeProduct><ram:Name>Wartung März</ram:Name></ram:SpecifiedTradeProduct>
      <ram:SpecifiedLineTradeAgreement><ram:NetPriceProductTradePrice><ram:ChargeAmount>80.00</ram:ChargeAmount></ram:NetPriceProductTradePrice></ram:SpecifiedLineTradeAgreement>
      <ram:SpecifiedLineTradeDelivery><ram:BilledQuantity unitCode="HUR">2</ram:BilledQuantity></ram:SpecifiedLineTradeDelivery>
      <ram:SpecifiedLineTradeSettlement>
        <ram:ApplicableTradeTax><ram:TypeCode>VAT</ram:TypeCode><ram:CategoryCode>S</ram:CategoryCode><ram:RateApplicablePercent>19</ram:RateApplicablePercent></ram:ApplicableTradeTax>
        <ram:SpecifiedTradeAllowanceCharge><ram:ChargeIndicator><udt:Indicator>false</udt:Indicator></ram:ChargeIndicator>
          <ram:CalculationPercent>5</ram:CalculationPercent><ram:BasisAmount>160.00</ram:BasisAmount><ram:ActualAmount>8.00</ram:ActualAmount><ram:Reason>Rabatt</ram:Reason></ram:SpecifiedTradeAllowanceCharge>
        <ram:SpecifiedTradeSettlementLineMonetarySummation><ram:LineTotalAmount>152.00</ram:LineTotalAmount></ram:SpecifiedTradeSettlementLineMonetarySummation>
      </ram:SpecifiedLineTradeSettlement>
    </ram:IncludedSupplyChainTradeLineItem>
    <ram:ApplicableHeaderTradeAgreement>
      <ram:BuyerReference>Muster-Einkauf</ram:BuyerReference>
      <ram:SellerTradeParty><ram:Name>Service Partner GmbH</ram:Name>
        <ram:DefinedTradeContact><ram:PersonName>Frau Beispiel</ram:PersonName>
          <ram:TelephoneUniversalCommunication><ram:CompleteNumber>+49 30 9876543</ram:CompleteNumber></ram:TelephoneUniversalCommunication>
          <ram:EmailURIUniversalCommunication><ram:URIID>info@service.example</ram:URIID></ram:EmailURIUniversalCommunication></ram:DefinedTradeContact>
        <ram:PostalTradeAddress><ram:PostcodeCode>10115</ram:PostcodeCode><ram:CityName>Berlin</ram:CityName><ram:CountryID>DE</ram:CountryID></ram:PostalTradeAddress>
        <ram:URIUniversalCommunication><ram:URIID schemeID="EM">info@service.example</ram:URIID></ram:URIUniversalCommunication>
        <ram:SpecifiedTaxRegistration><ram:ID schemeID="VA">DE222222222</ram:ID></ram:SpecifiedTaxRegistration>
      </ram:SellerTradeParty>
      <ram:BuyerTradeParty><ram:Name>Muster GmbH</ram:Name>
        <ram:PostalTradeAddress><ram:PostcodeCode>80331</ram:PostcodeCode><ram:CityName>München</ram:CityName><ram:CountryID>DE</ram:CountryID></ram:PostalTradeAddress>
        <ram:URIUniversalCommunication><ram:URIID schemeID="EM">rechnung@muster.example</ram:URIID></ram:URIUniversalCommunication>
      </ram:BuyerTradeParty>
    </ram:ApplicableHeaderTradeAgreement>
    <ram:ApplicableHeaderTradeDelivery><ram:ActualDeliverySupplyChainEvent><ram:OccurrenceDateTime><udt:DateTimeString format="102">20260331</udt:DateTimeString></ram:OccurrenceDateTime></ram:ActualDeliverySupplyChainEvent></ram:ApplicableHeaderTradeDelivery>
    <ram:ApplicableHeaderTradeSettlement>
      <ram:InvoiceCurrencyCode>EUR</ram:InvoiceCurrencyCode>
      <ram:SpecifiedTradeSettlementPaymentMeans><ram:TypeCode>58</ram:TypeCode>
        <ram:PayeePartyCreditorFinancialAccount><ram:IBANID>DE89370400440532013000</ram:IBANID></ram:PayeePartyCreditorFinancialAccount></ram:SpecifiedTradeSettlementPaymentMeans>
      <ram:ApplicableTradeTax><ram:CalculatedAmount>28.88</ram:CalculatedAmount><ram:TypeCode>VAT</ram:TypeCode><ram:BasisAmount>152.00</ram:BasisAmount><ram:CategoryCode>S</ram:CategoryCode><ram:RateApplicablePercent>19</ram:RateApplicablePercent></ram:ApplicableTradeTax>
      <ram:SpecifiedTradePaymentTerms><ram:Description>14 Tage netto</ram:Description></ram:SpecifiedTradePaymentTerms>
      <ram:SpecifiedTradeSettlementHeaderMonetarySummation>
        <ram:LineTotalAmount>152.00</ram:LineTotalAmount><ram:TaxBasisTotalAmount>152.00</ram:TaxBasisTotalAmount>
        <ram:TaxTotalAmount currencyID="EUR">28.88</ram:TaxTotalAmount><ram:GrandTotalAmount>180.88</ram:GrandTotalAmount>
        <ram:DuePayableAmount>180.88</ram:DuePayableAmount>
      </ram:SpecifiedTradeSettlementHeaderMonetarySummation>
    </ram:ApplicableHeaderTradeSettlement>
  </rsm:SupplyChainTradeTransaction>
</rsm:CrossIndustryInvoice>"#
    )
}

#[tokio::test]
async fn an_incoming_xrechnung_becomes_a_draft_vendor_invoice() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order_number = f.order["number"].as_str().unwrap();
    let xml = incoming_ubl(
        order_number,
        "RE-2026-0042",
        "DE111111111",
        "Lieferant GmbH",
    );

    // Read first: the vendor by VAT identifier (written with spaces in
    // its record), the order, the order line, the tax code.
    let (status, read) = t
        .command(
            &t.admin,
            "einvoice.read",
            json!({ "company_id": f.company, "content": xml }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["valid"], true, "{read}");
    assert_eq!(read["syntax"], "ubl");
    assert_eq!(read["vendor_id"], f.vendor.as_str());
    assert_eq!(read["candidates"][0]["matched_by"], "vat_id");
    assert_eq!(read["purchase_order_id"], f.order["id"]);
    let lines = read["lines"].as_array().unwrap();
    assert_eq!(lines[0]["order_line_id"], f.order["body"]["lines"][0]["id"]);
    assert_eq!(lines[0]["matched_by"], "line_reference");
    assert_eq!(lines[0]["tax_code_id"], f.vat19.as_str());
    assert_eq!(lines[1]["order_line_id"], Value::Null);

    // The packaging line needs an account.
    let (status, body) = t
        .command(
            &t.admin,
            "einvoice.import",
            json!({ "company_id": f.company, "content": xml }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "account_id");
    let (status, draft) = t
        .command(
            &t.admin,
            "einvoice.import",
            json!({ "company_id": f.company, "content": xml, "account_id": f.account["4930"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(draft["doc_type"], "vendor_invoice");
    assert_eq!(draft["status"], "draft");
    assert_eq!(draft["doc_date"], "2026-04-05");
    assert_eq!(draft["body"]["purchase_order_id"], f.order["id"]);
    assert_eq!(draft["body"]["vendor_reference"], "RE-2026-0042");
    let lines = draft["body"]["lines"].as_array().unwrap();
    assert_eq!(lines[0]["order_line_id"], f.order["body"]["lines"][0]["id"]);
    assert_eq!(dec(&lines[0]["qty"]), 5.0);
    assert_eq!(dec(&lines[0]["unit_price"]), 20.0);
    assert_eq!(lines[0]["tax_code_id"], f.vat19.as_str());
    assert_eq!(lines[1]["account_id"], f.account["4930"].as_str());
    assert_eq!(dec(&draft["summary"]["total_net"]), 105.0);
    assert_eq!(dec(&draft["summary"]["total_tax"]), 19.95);

    // The same invoice again is refused.
    let (status, body) = t
        .command(
            &t.admin,
            "einvoice.import",
            json!({ "company_id": f.company, "content": xml, "account_id": f.account["4930"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("imported before"),
        "{body}"
    );

    // An invoice addressed to another company's VAT identifier is refused.
    let other = xml
        .replace("DE123456789", "DE999999999")
        .replace("RE-2026-0042", "RE-X");
    let (status, read) = t
        .command(
            &t.admin,
            "einvoice.read",
            json!({ "company_id": f.company, "content": other }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["valid"], false);
    t.stop().await;
}

#[tokio::test]
async fn a_cii_invoice_of_an_unknown_seller_is_proposed_not_guessed() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let service = t
        .create(
            "vendor",
            json!({ "code": "V2", "name": "Service Partner GmbH" }),
        )
        .await;
    let xml = incoming_cii("SP-77");
    let (status, read) = t
        .command(
            &t.admin,
            "einvoice.read",
            json!({ "company_id": f.company, "content": xml }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["valid"], true, "{read}");
    assert_eq!(read["syntax"], "cii");
    assert_eq!(read["vendor_id"], Value::Null);
    assert_eq!(read["candidates"][0]["vendor_id"], service.as_str());
    assert_eq!(read["candidates"][0]["matched_by"], "name");
    // A name is not enough.
    let (status, body) = t
        .command(
            &t.admin,
            "einvoice.import",
            json!({ "company_id": f.company, "content": xml, "account_id": f.account["4930"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "vendor_id");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("V2 Service Partner GmbH")
    );
    // Confirmed, it is a direct invoice on the account, its discount kept.
    let (status, draft) = t
        .command(
            &t.admin,
            "einvoice.import",
            json!({ "company_id": f.company, "content": xml, "vendor_id": service,
                    "account_id": f.account["4930"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(draft["body"]["vendor_id"], service.as_str());
    assert_eq!(draft["body"]["purchase_order_id"], Value::Null);
    let line = &draft["body"]["lines"][0];
    assert_eq!(dec(&line["qty"]), 2.0);
    assert_eq!(dec(&line["unit_price"]), 80.0);
    assert_eq!(dec(&line["discount_amount"]), 8.0);
    assert_eq!(dec(&draft["summary"]["total_net"]), 152.0);
    assert_eq!(dec(&draft["summary"]["total_tax"]), 28.88);
    t.stop().await;
}

/// A minimal PDF with `xml` embedded under `name`, written by hand.
fn pdf_with(xml: &str, name: &str, javascript: bool) -> Vec<u8> {
    let mut objects = vec![
        format!(
            "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [({name}) 4 0 R] >> >>{} >>",
            if javascript { " /OpenAction 5 0 R" } else { "" }
        ),
        "<< /Type /Pages /Kids [] /Count 0 >>".to_owned(),
        format!(
            "<< /Type /EmbeddedFile /Subtype /text#2Fxml /Length {} >>\nstream\n{xml}\nendstream",
            xml.len()
        ),
        format!("<< /Type /Filespec /F ({name}) /UF ({name}) /EF << /F 3 0 R >> >>"),
    ];
    if javascript {
        objects.push("<< /S /JavaScript /JS (app.alert(1)) >>".to_owned());
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[tokio::test]
async fn a_zugferd_pdf_is_read_and_hostile_files_are_refused() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let read = |input: Value| {
        let t = &t;
        let mut input = input;
        input["company_id"] = json!(f.company);
        async move { t.command(&t.admin, "einvoice.read", input).await }
    };

    // The CII inside a ZUGFeRD / Factur-X PDF.
    let pdf = pdf_with(&incoming_cii("SP-78"), "factur-x.xml", false);
    let (status, body) = read(json!({ "content_base64": b64(&pdf) })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["container"], "pdf");
    assert_eq!(body["invoice"]["number"], "SP-78");
    // With JavaScript in it, the PDF is refused.
    let pdf = pdf_with(&incoming_cii("SP-79"), "factur-x.xml", true);
    let (status, body) = read(json!({ "content_base64": b64(&pdf) })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JavaScript"),
        "{body}"
    );
    // A PDF without an invoice in it.
    let pdf = pdf_with("<x/>", "notes.xml", false);
    let (status, body) = read(json!({ "content_base64": b64(&pdf) })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no embedded invoice")
    );

    // XML that would expand, reach out or is not an invoice.
    let refused = [
        (
            "<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\"><!ENTITY lol2 \"&lol;&lol;&lol;\">]><Invoice>&lol2;</Invoice>",
            "DTD",
        ),
        (
            "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><r>&x;</r>",
            "DTD",
        ),
        ("<Invoice>&x;</Invoice>", "unknown entity"),
        ("<Invoice><ID>1</Invoice>", "not well-formed"),
        ("<html><body>hello</body></html>", "not an EN 16931 invoice"),
        ("not xml at all", "outside the root element"),
    ];
    for (content, why) in refused {
        let (status, body) = read(json!({ "content": content })).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{content}: {body}"
        );
        assert!(
            body["error"]["message"].as_str().unwrap().contains(why),
            "{content}: {body}"
        );
    }
    // ZUGFeRD MINIMUM is not an invoice under EN 16931.
    let minimum = incoming_cii("SP-80").replace(
        "urn:cen.eu:en16931:2017#compliant#urn:xeinkauf.de:kosit:xrechnung_3.0",
        "urn:factur-x.eu:1p0:minimum",
    );
    let (status, body) = read(json!({ "content": minimum })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("MINIMUM")
    );
    // Arithmetic that does not add up is reported, and not imported.
    let wrong = incoming_cii("SP-81").replace(
        "<ram:GrandTotalAmount>180.88</ram:GrandTotalAmount>",
        "<ram:GrandTotalAmount>181.88</ram:GrandTotalAmount>",
    );
    let (status, body) = read(json!({ "content": wrong })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["valid"], false);
    let rules: Vec<&str> = body["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["fatal"] == true)
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"BR-CO-15"), "{rules:?}");
    let (status, body) = t
        .command(
            &t.admin,
            "einvoice.import",
            json!({ "company_id": f.company, "content": wrong, "account_id": f.account["4930"],
                    "vendor_id": f.vendor }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("BR-CO-15")
    );
    // Too large: the server's request limit refuses it before the
    // module's own (5 MiB of XML, 20 MiB of PDF) would.
    let big = "<a>".to_owned() + &"x".repeat(6 * 1024 * 1024) + "</a>";
    let (status, _) = read(json!({ "content": big })).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    t.stop().await;
}

/// Regression tests of the review of the import: hand-made files that
/// once panicked, slipped past a limit, or drafted something else than the
/// invoice said.
#[tokio::test]
async fn hostile_numbers_bombs_and_differences_are_caught() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let order_number = f.order["number"].as_str().unwrap().to_owned();
    let base = incoming_ubl(&order_number, "RE-R1", "DE111111111", "Lieferant GmbH");
    let read = |content: String| {
        let t = &t;
        let company = f.company.clone();
        async move {
            t.command(
                &t.admin,
                "einvoice.read",
                json!({ "company_id": company, "content": content }),
            )
            .await
        }
    };

    // 1. A number beyond what the books hold is refused when read; one
    // that would overflow in arithmetic is a finding, not a crash.
    let huge = base.replace(
        "<cbc:InvoicedQuantity unitCode=\"H87\">5</cbc:InvoicedQuantity>",
        "<cbc:InvoicedQuantity unitCode=\"H87\">79228162514264337593543950335</cbc:InvoicedQuantity>",
    );
    let (status, body) = read(huge).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("too large"),
        "{body}"
    );
    let tiny_base = base.replace(
        "<cbc:PriceAmount currencyID=\"EUR\">20.00</cbc:PriceAmount></cac:Price>",
        "<cbc:PriceAmount currencyID=\"EUR\">99999999999999</cbc:PriceAmount><cbc:BaseQuantity unitCode=\"H87\">0.0000000001</cbc:BaseQuantity></cac:Price>",
    );
    let (status, body) = read(tiny_base).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["valid"], false);

    // 2. A PDF whose object stream of a few kilobytes inflates beyond the
    // per-stream limit is refused while it is loaded.
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut e, b"4 0 ").unwrap();
    std::io::Write::write_all(&mut e, &vec![b' '; 5 * 1024 * 1024]).unwrap();
    let bomb = e.finish().unwrap();
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![];
    for body in [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec(),
        [
            format!(
                "<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {} >>\nstream\n",
                bomb.len()
            )
            .into_bytes(),
            bomb,
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ] {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", offsets.len()).as_bytes());
        pdf.extend(body);
        pdf.extend(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend(b"xref\n0 4\n0000000000 65535 f \n");
    for o in offsets {
        pdf.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    let (status, body) = t
        .command(
            &t.admin,
            "einvoice.read",
            json!({ "company_id": f.company,
                    "content_base64": base64::engine::general_purpose::STANDARD.encode(&pdf) }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("could not be read"),
        "{body}"
    );

    // 3. The VAT stated a cent off what the draft would post: listed, and
    // drafted only when accepted, with the difference in the memo.
    let cent = base
        .replace("RE-R1", "RE-R3")
        .replace(">19.95<", ">19.96<")
        .replace(">124.95<", ">124.96<");
    let (status, body) = read(cent.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dec(&body["draft"]["tax"]), 19.95);
    let differences = body["differences"].as_array().unwrap();
    assert!(
        differences
            .iter()
            .any(|d| d.as_str().unwrap().contains("BT-110")),
        "{body}"
    );
    let import = |content: String, extra: Value| {
        let t = &t;
        let mut input = json!({ "company_id": f.company, "content": content,
                                "account_id": f.account["4930"] });
        for (k, v) in extra.as_object().unwrap() {
            input[k] = v.clone();
        }
        async move { t.command(&t.admin, "einvoice.import", input).await }
    };
    let (status, body) = import(cent.clone(), json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "accept_differences");
    let (status, draft) = import(cent, json!({ "accept_differences": true })).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert!(
        draft["memo"]
            .as_str()
            .unwrap()
            .contains("Differences accepted"),
        "{draft}"
    );
    // Paid in part already: said, not drafted silently.
    let prepaid = base
        .replace("RE-R1", "RE-R4")
        .replace(
            "<cbc:PayableAmount currencyID=\"EUR\">124.95</cbc:PayableAmount>",
            "<cbc:PrepaidAmount currencyID=\"EUR\">24.95</cbc:PrepaidAmount><cbc:PayableAmount currencyID=\"EUR\">100.00</cbc:PayableAmount>",
        );
    let (status, body) = read(prepaid).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["differences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("BT-113")),
        "{body}"
    );
    // VAT at a rate the company has no code for: a problem, not a draft
    // without tax.
    let foreign_rate = base
        .replace("RE-R1", "RE-R5")
        .replace(
            "<cbc:Percent>19</cbc:Percent>",
            "<cbc:Percent>16</cbc:Percent>",
        )
        .replace(">19.95<", ">16.80<")
        .replace(">124.95<", ">121.80<");
    let (status, body) = read(foreign_rate.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let problems = body["problems"].as_array().unwrap();
    assert!(problems[0].as_str().unwrap().contains("16% VAT"), "{body}");
    let (status, body) = import(foreign_rate, json!({ "accept_differences": true })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("line_tax_codes"),
        "{body}"
    );

    // 4. A price per base quantity: 7 at 10.00 per 3 is 23.33; the draft
    // comes to exactly that.
    let per_base = base
        .replace("RE-R1", "RE-R6")
        .replace(
            "<cbc:InvoicedQuantity unitCode=\"C62\">1</cbc:InvoicedQuantity>\n    <cbc:LineExtensionAmount currencyID=\"EUR\">5.00</cbc:LineExtensionAmount>",
            "<cbc:InvoicedQuantity unitCode=\"C62\">7</cbc:InvoicedQuantity>\n    <cbc:LineExtensionAmount currencyID=\"EUR\">23.33</cbc:LineExtensionAmount>",
        )
        .replace(
            "<cbc:PriceAmount currencyID=\"EUR\">5.00</cbc:PriceAmount></cac:Price>",
            "<cbc:PriceAmount currencyID=\"EUR\">10.00</cbc:PriceAmount><cbc:BaseQuantity unitCode=\"C62\">3</cbc:BaseQuantity></cac:Price>",
        )
        .replace(">105.00<", ">123.33<")
        .replace(">19.95<", ">23.43<")
        .replace(">124.95<", ">146.76<");
    let (status, draft) = import(per_base, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let line = &draft["body"]["lines"][1];
    assert_eq!(dec(&line["qty"]), 7.0);
    assert_eq!(line["unit_price"], "3.333334");
    assert_eq!(dec(&draft["summary"]["total_net"]), 123.33);
    assert_eq!(dec(&draft["summary"]["total_tax"]), 23.43);

    // 5. Someone who may draft vendor invoices but not read vendors or
    // orders is proposed neither.
    let (clerk, _) = t
        .user_with_rules(
            "ap@example.com",
            json!(["vendor_invoice:create"]),
            Some(&f.company),
        )
        .await;
    let (status, body) = t
        .command(
            &clerk,
            "einvoice.read",
            json!({ "company_id": f.company, "content": base.replace("RE-R1", "RE-R7") }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["candidates"], json!([]));
    assert_eq!(body["vendor_id"], Value::Null);
    assert_eq!(body["purchase_order_id"], Value::Null);

    // 6. A buyer named otherwise, without a VAT identifier, is said.
    let unnamed = base
        .replace("RE-R1", "RE-R8")
        .replace(
            "<cac:PartyTaxScheme><cbc:CompanyID>DE123456789</cbc:CompanyID><cac:TaxScheme><cbc:ID>VAT</cbc:ID></cac:TaxScheme></cac:PartyTaxScheme>\n    <cac:PartyLegalEntity><cbc:RegistrationName>Muster GmbH</cbc:RegistrationName>",
            "<cac:PartyLegalEntity><cbc:RegistrationName>Andere AG</cbc:RegistrationName>",
        )
        .replace("rechnung@muster.example</cbc:EndpointID>", "info@andere.example</cbc:EndpointID>");
    let (status, body) = read(unnamed).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["differences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("identifies the company")),
        "{body}"
    );

    // 7. A date with a time zone is a date.
    let zoned = base.replace("RE-R1", "RE-R9").replace(
        "<cbc:IssueDate>2026-04-05</cbc:IssueDate>",
        "<cbc:IssueDate>2026-04-05+02:00</cbc:IssueDate>",
    );
    let (status, body) = read(zoned).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["invoice"]["issue_date"], "2026-04-05");
    t.stop().await;
}

/// A posted invoice keeps the addresses it was posted with: after the
/// customer moves, it still prints, and is still sent, to the old one.
/// A document may name one of the customer's addresses, and only one of
/// theirs; a draft follows the customer until it is posted.
#[tokio::test]
async fn a_posted_invoice_keeps_the_address_it_was_posted_with() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let invoice = f.invoice(&t, &f.customer).await;
    // The purchase order of the stock kept its vendor as seller and the
    // company as buyer when it was approved; its receipt kept the same.
    let kept = t
        .count(&format!(
            "SELECT count(*) FROM document_party p JOIN document d ON d.id = p.document_id
             WHERE d.doc_type IN ('purchase_order', 'purchase_receipt')
               AND ((p.role = 'seller' AND p.party_type = 'vendor')
                    OR (p.role = 'buyer' AND p.party_type = 'company'))
               AND d.company_id = '{}'",
            f.company
        ))
        .await;
    assert_eq!(kept, 4);

    // The customer moves, and gets a warehouse to ship to.
    let (status, moved) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer", "party_id": f.customer, "addresses": [
                { "kind": "billing", "street": "Neue Straße 7", "city": "Bonn",
                  "postcode": "53111", "country_code": "DE", "is_default": true },
                { "kind": "shipping", "street": "Lagerweg 2", "city": "Hürth",
                  "postcode": "50354", "country_code": "DE", "is_default": true },
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");

    // The posted invoice: its parties, its page and its e-invoice show the
    // address it was posted with.
    let (status, parties) = t
        .command(
            &t.admin,
            "document.parties",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{parties}");
    assert_eq!(parties["buyer"]["street"], "Marktplatz 5");
    assert_eq!(parties["buyer"]["city"], "Köln");
    assert_eq!(parties["buyer"]["vat_id"], "DE987654321");
    assert_eq!(parties["seller"]["city"], "München");
    // It had no shipping address then, so it shows none now.
    assert!(parties.get("ship_to").is_none(), "{parties}");
    let (status, page) = t
        .command(
            &t.admin,
            "document.render",
            json!({ "doc_type": "sales_invoice", "id": invoice["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let html = page["html"].as_str().unwrap();
    assert!(html.contains("Marktplatz 5"), "the old address is printed");
    assert!(!html.contains("Neue Straße 7"), "the new address is not");
    let (status, out) = export(&t, "sales_invoice", &invoice, "ubl").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let parsed = read(out["xml"].as_str().unwrap());
    assert_eq!(parsed.buyer.city.as_deref(), Some("Köln"));
    assert_eq!(parsed.buyer.street.as_deref(), Some("Marktplatz 5"));
    // A credit note of it keeps the invoice's parties.
    let line = invoice["body"]["lines"][2]["id"].clone();
    let credit = f
        .posted(
            &t,
            "sales_credit_note",
            "2026-04-10",
            json!({ "sales_invoice_id": invoice["id"],
                    "lines": [{ "invoice_line_id": line, "qty": "1" }] }),
        )
        .await;
    let (status, out) = export(&t, "sales_credit_note", &credit, "cii").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        read(out["xml"].as_str().unwrap()).buyer.city.as_deref(),
        Some("Köln")
    );

    // The addresses on record now, with their ids.
    let (_, records) = t
        .command(
            &t.admin,
            "party.get",
            json!({ "party_type": "customer", "party_id": f.customer }),
        )
        .await;
    let bonn = records["addresses"][0]["id"].as_str().unwrap().to_owned();
    let hurth = records["addresses"][1]["id"].as_str().unwrap().to_owned();
    assert_eq!(records["addresses"][0]["city"], "Bonn");
    // Only the shipping ones, when asked.
    let (_, shipping) = t
        .command(
            &t.admin,
            "party.get",
            json!({ "party_type": "customer", "party_id": f.customer,
                    "address_kind": "shipping" }),
        )
        .await;
    assert_eq!(shipping["addresses"].as_array().unwrap().len(), 1);
    assert_eq!(shipping["addresses"][0]["id"], hurth.as_str());
    // Given back with its id, an address keeps it; a third is added.
    let (status, kept) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer", "party_id": f.customer, "addresses": [
                records["addresses"][0], records["addresses"][1],
                { "kind": "billing", "street": "Filiale 3", "city": "Aachen",
                  "country_code": "DE" },
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["addresses"][0]["id"], bonn);
    let aachen = kept["addresses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["city"] == "Aachen")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // An order may name an address of its customer, and no other party's.
    let (_, other) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "vendor", "party_id": f.vendor, "addresses": [
                { "street": "Fremd 1", "country_code": "DE" } ] }),
        )
        .await;
    let foreign = other["addresses"][0]["id"].clone();
    let order_body = |billing: Value| {
        json!({ "company_id": f.company, "doc_date": "2026-05-01", "body": {
            "customer_id": f.customer, "billing_address_id": billing,
            "lines": [{ "item_id": f.widget, "warehouse_id": f.warehouse, "qty": "1",
                        "unit_price": "25.00" }] } })
    };
    let (status, refused) = t
        .post(&t.admin, "/api/documents/sales_order", order_body(foreign))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused.to_string().contains("billing_address_id"),
        "{refused}"
    );
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            order_body(json!(aachen)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    assert_eq!(draft["body"]["billing_address_id"], aachen.as_str());
    let (status, order) = t
        .command(
            &t.admin,
            "sales_order.approve",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    // Its invoice takes the order's address unless it names its own, and
    // ships to the customer's default shipping address.
    let line = order["body"]["lines"][0]["id"].clone();
    f.posted(
        &t,
        "sales_delivery",
        "2026-05-02",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "1" }] }),
    )
    .await;
    let second = f
        .posted(
            &t,
            "sales_invoice",
            "2026-05-03",
            json!({ "sales_order_id": order["id"], "lines": [
                { "order_line_id": line, "qty": "1", "unit_price": "25.00",
                  "tax_code_id": f.vat19 } ] }),
        )
        .await;
    assert_eq!(second["body"]["billing_address_id"], aachen.as_str());
    assert_eq!(second["body"]["shipping_address_id"], Value::Null);
    let (_, parties) = t
        .command(
            &t.admin,
            "document.parties",
            json!({ "doc_type": "sales_invoice", "id": second["id"] }),
        )
        .await;
    assert_eq!(parties["buyer"]["city"], "Aachen");
    assert_eq!(parties["ship_to"]["city"], "Hürth");
    // A posted document's copy cannot be changed.
    let changed = sqlx::query("UPDATE document_party SET city = 'X' WHERE document_id = $1")
        .bind(uuid::Uuid::parse_str(second["id"].as_str().unwrap()).unwrap())
        .execute(&t.pool)
        .await;
    assert!(changed.is_err(), "the copy is locked with its document");

    // An address a draft names that is removed before it is posted is
    // refused at posting, on the field that names it.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/sales_invoice",
            json!({ "company_id": f.company, "doc_date": "2026-05-04", "body": {
                "customer_id": f.customer, "billing_address_id": aachen,
                "lines": [{ "account_id": f.account["8400"], "qty": "1", "unit_price": "10.00",
                            "tax_code_id": f.vat19 }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, _) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer", "party_id": f.customer, "addresses": [
                { "id": bonn, "kind": "billing", "street": "Neue Straße 7", "city": "Bonn",
                  "country_code": "DE", "is_default": true } ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, refused) = t
        .command(&t.admin, "sales_invoice.post", json!({ "id": draft["id"] }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(
        refused.to_string().contains("billing_address_id"),
        "{refused}"
    );
    // The earlier invoice still shows Aachen, the address it was posted with.
    let (_, parties) = t
        .command(
            &t.admin,
            "document.parties",
            json!({ "doc_type": "sales_invoice", "id": second["id"] }),
        )
        .await;
    assert_eq!(parties["buyer"]["city"], "Aachen");
    t.stop().await;
}

/// A sale under the reverse charge is written as category AE at 0 %, with
/// the exemption reason, and the buyer's VAT identifier.
#[tokio::test]
async fn a_reverse_charge_sale_is_category_ae() {
    let t = TestApp::start().await;
    let f = setup(&t).await;
    let reverse = t
        .create(
            "tax_code",
            json!({ "code": "DE-RC", "name": "Steuerschuldnerschaft des Leistungsempfängers",
                    "rate": "19", "self_assessed": true }),
        )
        .await;
    let accounts: (String, String) = sqlx::query_as(
        "SELECT collected_account_id::text, paid_account_id::text FROM tax_settings
         WHERE tax_code_id = $1::uuid",
    )
    .bind(&f.vat19)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    t.create(
        "tax_settings",
        json!({ "company_id": f.company, "tax_code_id": reverse,
                "collected_account_id": accounts.0, "paid_account_id": accounts.1 }),
    )
    .await;
    let invoice = f
        .posted(
            &t,
            "sales_invoice",
            "2026-04-03",
            json!({ "customer_id": f.customer, "lines": [
                { "account_id": f.account["8400"], "description": "Montage", "qty": "1",
                  "unit_price": "500.00", "tax_code_id": reverse } ] }),
        )
        .await;
    assert_eq!(dec(&invoice["summary"]["total_gross"]), 500.0, "{invoice}");
    let (status, out) = export(&t, "sales_invoice", &invoice, "ubl").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let parsed = read(out["xml"].as_str().unwrap());
    assert_eq!(parsed.vat.len(), 1);
    assert_eq!(parsed.vat[0].category, "AE");
    assert_eq!(amount(parsed.vat[0].tax), 0.0);
    assert_eq!(amount(parsed.totals.payable), 500.0);
    assert_eq!(
        parsed.vat[0].exemption_reason_code.as_deref(),
        Some("VATEX-EU-AE")
    );
    t.stop().await;
}
