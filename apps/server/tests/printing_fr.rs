//! Printed documents in French, for a customer whose profile says `fr`:
//! French numbers (`1 234,56`) and dates (`15 mars 2026`); for a seller in
//! Canada what Quebec asks of an invoice (the GST/HST and QST registration
//! numbers, the taxes as TPS and TVQ), for a seller in France what French
//! law asks (SIREN, the VAT identifiers of both parties, the late-payment
//! penalties and the 40 € recovery indemnity).

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

/// A no-break space, which sets thousands apart in French, as the page
/// writes it.
const NBSP: &str = "&nbsp;";

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn page(t: &TestApp, doc_type: &str, id: &str) -> String {
    let rendered = run(
        t,
        "document.render",
        json!({ "doc_type": doc_type, "id": id }),
    )
    .await;
    rendered["html"].as_str().unwrap().to_owned()
}

struct Sale {
    order: Value,
    delivery: Value,
    invoice: Value,
}

/// A company from `template`, written to in French, with stock of one
/// item, that sells ten of it at 100 to a French-speaking customer: the
/// order of March 1, delivered on March 10, invoiced on March 15 with
/// `tax_code`.
async fn sale(
    t: &TestApp,
    template: &str,
    company: Value,
    customer: Value,
    tax_code: &str,
) -> (String, String, Sale) {
    let created = run(
        t,
        "company.setup",
        json!({ "template": template, "code": template.to_uppercase(), "name": "Exemple",
                "first_year": 2026 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let warehouse = created["warehouse_id"].clone();
    let mut company = company;
    company["party_type"] = json!("company");
    company["party_id"] = json!(c);
    run(t, "party.set", company).await;
    let terms = t
        .create(
            "payment_terms",
            json!({ "code": format!("{template}-2-10-30"), "name": "2 % 10, net 30",
                    "net_days": 30, "discount_percent": "2", "discount_days": 10 }),
        )
        .await;
    let client = t
        .create(
            "customer",
            json!({ "code": format!("{template}-CLIENT"), "name": "Client inc.",
                    "payment_terms_id": terms }),
        )
        .await;
    let mut customer = customer;
    customer["party_type"] = json!("customer");
    customer["party_id"] = json!(client);
    run(t, "party.set", customer).await;
    let item = t
        .create(
            "item",
            json!({ "code": format!("{template}-W"), "name": "Widget" }),
        )
        .await;
    let tax = t
        .get(&t.admin, &format!("/api/entities/tax_code?code={tax_code}"))
        .await
        .1["items"][0]["id"]
        .clone();
    assert!(tax.is_string(), "no tax code {tax_code}");
    let (_, receipt) = t
        .post(
            &t.admin,
            "/api/documents/stock_adjustment",
            json!({ "company_id": c, "doc_date": "2026-02-01", "body": {
                "offset_account_id": created["roles"]["inventory"]["id"],
                "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "100",
                            "unit_cost": "50" }] } }),
        )
        .await;
    run(t, "stock_adjustment.post", json!({ "id": id(&receipt) })).await;
    let (_, order) = t
        .post(
            &t.admin,
            "/api/documents/sales_order",
            json!({ "company_id": c, "doc_date": "2026-03-01", "body": {
                "customer_id": client, "lines": [{ "item_id": item,
                "warehouse_id": warehouse, "qty": "10", "unit_price": "100" }] } }),
        )
        .await;
    let order = run(t, "sales_order.approve", json!({ "id": id(&order) })).await;
    let delivery = run(
        t,
        "sales_order.deliver",
        json!({ "id": id(&order), "doc_date": "2026-03-10" }),
    )
    .await;
    run(t, "sales_delivery.post", json!({ "id": id(&delivery) })).await;
    let mut invoice = run(
        t,
        "sales_order.invoice",
        json!({ "id": id(&order), "doc_date": "2026-03-15" }),
    )
    .await;
    invoice["body"]["lines"][0]["tax_code_id"] = tax;
    let (status, invoice) = t
        .patch(
            &t.admin,
            &format!("/api/documents/sales_invoice/{}", id(&invoice)),
            json!({ "version": invoice["version"], "body": invoice["body"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{invoice}");
    let invoice = run(t, "sales_invoice.post", json!({ "id": id(&invoice) })).await;
    (
        c,
        client,
        Sale {
            order,
            delivery,
            invoice,
        },
    )
}

fn has_all(html: &str, wanted: &[&str], what: &str) {
    for wanted in wanted {
        assert!(
            html.contains(wanted),
            "{wanted:?} is not on the {what}:\n{html}"
        );
    }
}

#[tokio::test]
async fn a_quebec_customer_gets_a_french_invoice_with_gst_and_qst() {
    let t = TestApp::start().await;
    let (c, client, sale) = sale(
        &t,
        "ca_small_business",
        json!({ "profile": { "language": "fr" },
                "addresses": [{ "kind": "billing", "is_default": true,
                                "street": "1000, rue Sherbrooke Ouest", "city": "Montréal",
                                "region": "QC", "postcode": "H3A 3G4", "country_code": "CA" }],
                "tax_ids": [{ "scheme": "ca_bn", "value": "123456789RT0001" },
                            { "scheme": "ca_qst", "value": "1234567890TQ0001" }] }),
        json!({ "addresses": [{ "kind": "billing", "is_default": true,
                                "street": "500, Grande Allée Est", "city": "Québec",
                                "region": "QC", "postcode": "G1R 2J7", "country_code": "CA" }] }),
        "CA-GST-QST",
    )
    .await;
    let html = page(&t, "sales_invoice", &id(&sale.invoice)).await;
    has_all(
        &html,
        &[
            "Facture",
            "N° de facture",
            "15 mars 2026",
            "Date de livraison",
            "10 mars 2026",
            "TPS/TVH n° 123456789RT0001",
            "TVQ n° 1234567890TQ0001",
            "Montréal (QC) H3A 3G4",
            "Québec (QC) G1R 2J7",
            &format!("TPS 5 % sur 1{NBSP}000,00"),
            "50,00",
            &format!("TVQ 9,975 % sur 1{NBSP}000,00"),
            "99,75",
            &format!("1{NBSP}149,75"),
            "Payable au plus tard le 14 avril 2026",
            "Escompte de",
        ],
        "Quebec invoice",
    );
    // France's mentions are not Quebec's.
    for absent in ["TVA intracommunautaire", "40 €", "SIREN"] {
        assert!(!html.contains(absent), "{absent:?} on a Quebec invoice");
    }
    assert!(
        page(&t, "sales_delivery", &id(&sale.delivery))
            .await
            .contains("Bon de livraison")
    );
    let order = page(&t, "sales_order", &id(&sale.order)).await;
    assert!(
        order.contains("Confirmation de commande") && order.contains("TPS/TVH, TVQ"),
        "{order}"
    );
    let (_, statement) = t
        .post(
            &t.admin,
            "/api/documents/customer_statement",
            json!({ "company_id": c, "doc_date": "2026-03-31",
                    "body": { "customer_id": client, "from_date": "2026-03-01" } }),
        )
        .await;
    let html = page(&t, "customer_statement", &id(&statement)).await;
    has_all(
        &html,
        &[
            "Relevé de compte",
            "Solde d'ouverture",
            "Du 1er mars 2026 au 31 mars 2026",
        ],
        "statement",
    );
    // The PDF is made of the same page.
    let pdf = run(
        &t,
        "document.render_pdf",
        json!({ "doc_type": "sales_invoice", "id": id(&sale.invoice) }),
    )
    .await;
    assert!(pdf["data"].as_str().is_some_and(|d| !d.is_empty()));
    t.stop().await;
}

#[tokio::test]
async fn a_french_customer_gets_an_invoice_with_what_french_law_asks() {
    let t = TestApp::start().await;
    // A SIREN is nine digits.
    let (status, body) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "customer",
                    "party_id": t.create("customer", json!({ "code": "X", "name": "X" })).await,
                    "tax_ids": [{ "scheme": "fr_siren", "value": "12345" }] }),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");

    let (_, _, sale) = sale(
        &t,
        "de_skr03",
        json!({ "profile": { "language": "fr", "legal_registration": "SAS au capital de 10 000 €, RCS Paris 123 456 789" },
                "addresses": [{ "kind": "billing", "is_default": true,
                                "street": "10, rue de Rivoli", "city": "Paris",
                                "postcode": "75001", "country_code": "FR" }],
                "tax_ids": [{ "scheme": "fr_siren", "value": "123 456 789" },
                            { "scheme": "eu_vat", "value": "FR12123456789" }] }),
        json!({ "addresses": [{ "kind": "billing", "is_default": true,
                                "street": "Rue de la Loi 16", "city": "Bruxelles",
                                "postcode": "1000", "country_code": "BE" }],
                "tax_ids": [{ "scheme": "eu_vat", "value": "BE0123456789" }] }),
        "DE-USt19",
    )
    .await;
    let html = page(&t, "sales_invoice", &id(&sale.invoice)).await;
    has_all(
        &html,
        &[
            "Facture",
            "Date de facture",
            "15 mars 2026",
            "Date de livraison",
            "10 mars 2026",
            "SIREN 123 456 789",
            "N° TVA intracommunautaire FR12123456789",
            "N° TVA intracommunautaire BE0123456789",
            "1000 Bruxelles, BE",
            &format!("TVA 19 % sur 1{NBSP}000,00"),
            "190,00",
            &format!("Total TTC EUR</th><td class=\"num\">1{NBSP}190,00"),
            "Payable au plus tard le 14 avril 2026",
            "pénalités",
            "L. 441-10",
            "indemnité forfaitaire pour frais de recouvrement de 40 €",
            "RCS Paris",
        ],
        "French invoice",
    );
    assert!(!html.contains("TPS"), "{html}");
    t.stop().await;
}
