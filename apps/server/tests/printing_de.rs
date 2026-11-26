//! Printed documents in the party's language: German for a customer whose
//! profile says `de` (else the company's language), with German numbers
//! and dates and what § 14 UStG asks of an invoice; English otherwise. A
//! company's own template of a language wins over the built-in one.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

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

#[tokio::test]
async fn a_german_customer_gets_a_german_invoice() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "de_skr03", "code": "MUSTER", "name": "Muster GmbH",
                "first_year": 2026 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let warehouse = created["warehouse_id"].clone();
    run(
        &t,
        "party.set",
        json!({ "party_type": "company", "party_id": c,
                "profile": { "language": "de", "legal_registration": "HRB 12345, Amtsgericht Berlin" },
                "addresses": [{ "kind": "billing", "is_default": true, "street": "Hauptstraße 1",
                                "city": "Berlin", "postcode": "10115", "country_code": "DE" }],
                "tax_ids": [{ "scheme": "tax_number", "value": "27/123/45678" },
                            { "scheme": "eu_vat", "value": "DE123456789" }] }),
    )
    .await;
    let terms = t
        .create(
            "payment_terms",
            json!({ "code": "2-10-30", "name": "2 % 10, 30 netto", "net_days": 30,
                    "discount_percent": "2", "discount_days": 10 }),
        )
        .await;
    let kunde = t
        .create(
            "customer",
            json!({ "code": "KUNDE", "name": "Kunde AG", "payment_terms_id": terms }),
        )
        .await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "customer", "party_id": kunde,
                "addresses": [{ "kind": "billing", "is_default": true, "street": "Marktplatz 2",
                                "city": "München", "postcode": "80331", "country_code": "DE" }] }),
    )
    .await;
    let buyer = t
        .create("customer", json!({ "code": "BUYER", "name": "Buyer Ltd" }))
        .await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "customer", "party_id": buyer, "profile": { "language": "en" } }),
    )
    .await;
    let item = t
        .create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let vat = t
        .get(&t.admin, "/api/entities/tax_code?code=DE-USt19")
        .await
        .1["items"][0]["id"]
        .clone();
    // Bought, then sold, delivered on March 10 and invoiced on March 15.
    let sell = |customer: String| {
        let (t, c, warehouse, item, vat) =
            (&t, c.clone(), warehouse.clone(), item.clone(), vat.clone());
        async move {
            let (_, order) = t
                .post(
                    &t.admin,
                    "/api/documents/sales_order",
                    json!({ "company_id": c, "doc_date": "2026-03-01", "body": {
                        "customer_id": customer, "lines": [{ "item_id": item,
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
            invoice["body"]["lines"][0]["tax_code_id"] = vat;
            let (status, invoice) = t
                .patch(
                    &t.admin,
                    &format!("/api/documents/sales_invoice/{}", id(&invoice)),
                    json!({ "version": invoice["version"], "body": invoice["body"] }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{invoice}");
            let invoice = run(t, "sales_invoice.post", json!({ "id": id(&invoice) })).await;
            (order, delivery, invoice)
        }
    };
    // Stock to sell.
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
    run(&t, "stock_adjustment.post", json!({ "id": id(&receipt) })).await;
    let (order, delivery, invoice) = sell(kunde.clone()).await;

    let html = page(&t, "sales_invoice", &id(&invoice)).await;
    for wanted in [
        "Rechnung",
        "Rechnungsnummer",
        "Rechnungsdatum",
        "15.03.2026",
        "Leistungsdatum",
        "10.03.2026",
        "Steuernummer 27/123/45678",
        "USt-IdNr. DE123456789",
        "München",
        "Umsatzsteuer 19 % auf 1.000,00",
        "190,00",
        "1.190,00",
        "Zahlbar bis 14.04.2026 ohne Abzug",
        "Skonto",
        "HRB 12345",
    ] {
        assert!(
            html.contains(wanted),
            "{wanted:?} is not on the German invoice:\n{html}"
        );
    }
    assert!(
        page(&t, "sales_delivery", &id(&delivery))
            .await
            .contains("Lieferschein")
    );
    assert!(
        page(&t, "sales_order", &id(&order))
            .await
            .contains("Auftragsbestätigung")
    );
    // The PDF is made of the same page.
    let pdf = run(
        &t,
        "document.render_pdf",
        json!({ "doc_type": "sales_invoice", "id": id(&invoice) }),
    )
    .await;
    assert!(pdf["data"].as_str().is_some_and(|d| !d.is_empty()));

    // A customer that reads English, though the company writes German.
    let (_, _, english) = sell(buyer.clone()).await;
    let html = page(&t, "sales_invoice", &id(&english)).await;
    assert!(html.contains("Bill to"), "{html}");
    assert!(html.contains("1,190.00"), "{html}");

    // A statement in German.
    let (_, statement) = t
        .post(
            &t.admin,
            "/api/documents/customer_statement",
            json!({ "company_id": c, "doc_date": "2026-03-31",
                    "body": { "customer_id": kunde, "from_date": "2026-03-01" } }),
        )
        .await;
    let html = page(&t, "customer_statement", &id(&statement)).await;
    assert!(
        html.contains("Kontoauszug") && html.contains("Anfangssaldo"),
        "{html}"
    );

    // The company's own German invoice template wins for German customers.
    t.create(
        "print_template",
        json!({ "doc_type": "sales_invoice", "name": "Eigene Rechnung", "locale": "de",
                "is_default": true, "body": "<p>Unsere Rechnung {{ doc.number }}</p>" }),
    )
    .await;
    assert!(
        page(&t, "sales_invoice", &id(&invoice))
            .await
            .contains("Unsere Rechnung")
    );
    assert!(
        page(&t, "sales_invoice", &id(&english))
            .await
            .contains("Bill to")
    );
    t.stop().await;
}
