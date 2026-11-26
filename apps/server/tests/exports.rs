//! Accountant exports: DATEV, information returns, the general ledger.

mod common;

use axum::http::StatusCode;
use base64::Engine;
use common::{TestApp, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn account(t: &TestApp, company: &str, code: &str) -> String {
    let (_, list) = t
        .get(
            &t.admin,
            &format!("/api/entities/account?company_id={company}&code={code}"),
        )
        .await;
    list["items"][0]["id"].as_str().unwrap().to_owned()
}

async fn tax_code(t: &TestApp, code: &str) -> String {
    let (_, list) = t
        .get(&t.admin, &format!("/api/entities/tax_code?code={code}"))
        .await;
    list["items"][0]["id"].as_str().unwrap().to_owned()
}

/// A posted invoice without an order on one account line.
async fn invoice(
    t: &TestApp,
    doc_type: &str,
    company: &str,
    party: (&str, &str),
    date: &str,
    line: Value,
) -> Value {
    let mut body = json!({ "lines": [line] });
    body[party.0] = json!(party.1);
    if doc_type == "vendor_invoice" {
        body["vendor_reference"] = json!(format!("V-{date}-{}", party.1));
    }
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": company, "doc_date": date, "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(t, &format!("{doc_type}.post"), json!({ "id": id(&draft) })).await
}

/// Pay or take the money of an invoice's open item.
async fn pay(t: &TestApp, company: &str, bank: &Value, invoice: &Value, date: &str) {
    let item = invoice["postings"][0]["entries"]["open_items"][0]["id"].clone();
    let draft = run(
        t,
        "payment.draft",
        json!({ "company_id": company, "open_item_ids": [item], "bank_account_id": bank,
                "doc_date": date }),
    )
    .await;
    run(t, "payment.post", json!({ "id": draft["id"] })).await;
}

fn lines(file: &Value) -> Vec<String> {
    file["text"]
        .as_str()
        .unwrap()
        .split("\r\n")
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn a_german_company_exports_its_postings_and_parties_for_datev() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "de_skr03", "code": "MUSTER", "name": "Muster GmbH",
                "first_year": 2026 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let bank = created["roles"]["bank"]["id"].clone();
    let kunde = t
        .create(
            "customer",
            json!({ "code": "MUELLER", "name": "Müller GmbH" }),
        )
        .await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "customer", "party_id": kunde,
                "addresses": [{ "kind": "billing", "is_default": true, "street": "Marktplatz 2",
                                "city": "München", "postcode": "80331", "country_code": "DE" }],
                "tax_ids": [{ "scheme": "eu_vat", "value": "DE123456789" }] }),
    )
    .await;
    // Without settings there is nothing to export.
    let (status, body) = t
        .command(
            &t.admin,
            "datev.export_postings",
            json!({ "company_id": c, "from": "2026-03-01", "to": "2026-03-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    t.create(
        "datev_settings",
        json!({ "company_id": c, "consultant_number": 1001, "client_number": 456 }),
    )
    .await;

    let erloese = account(&t, &c, "8400").await;
    let sold = invoice(
        &t,
        "sales_invoice",
        &c,
        ("customer_id", &kunde),
        "2026-03-10",
        json!({ "account_id": erloese, "description": "Beratung", "qty": "1",
                "unit_price": "100.00", "tax_code_id": tax_code(&t, "DE-USt19").await }),
    )
    .await;
    pay(&t, &c, &bank, &sold, "2026-03-20").await;

    let file = run(
        &t,
        "datev.export_postings",
        json!({ "company_id": c, "from": "2026-03-01", "to": "2026-03-31",
                "description": "März 2026" }),
    )
    .await;
    let all = lines(&file);
    let header: Vec<&str> = all[0].split(';').collect();
    assert_eq!(header.len(), 31, "{}", all[0]);
    assert_eq!(
        header[..5],
        ["\"EXTF\"", "700", "21", "\"Buchungsstapel\"", "13"]
    );
    assert_eq!(
        header[10..16],
        ["1001", "456", "20260101", "4", "20260301", "20260331"]
    );
    assert_eq!(header[16], "\"März 2026\"");
    assert_eq!(header[21], "\"EUR\"");
    assert_eq!(all[1].split(';').count(), 125);
    assert!(all[1].starts_with("Umsatz (ohne Soll/Haben-Kz);Soll/Haben-Kennzeichen;"));
    // The invoice: 119,00 on 8400 against the customer's personal account,
    // tax folded in; 8400 is an automatic account in SKR 03, so no key.
    let rows: Vec<Vec<&str>> = all[2..].iter().map(|l| l.split(';').collect()).collect();
    assert_eq!(rows.len(), 2, "{all:?}");
    let sale = rows.iter().find(|r| r[6] == "8400").expect("a row on 8400");
    assert_eq!(sale[0], "119,00");
    assert_eq!(sale[1], "\"H\"");
    assert_eq!(sale[7], "10000");
    assert_eq!(sale[8], "");
    assert_eq!(sale[9], "1003");
    assert!(sale[10].starts_with('"') && sale[10].len() > 2, "{sale:?}");
    // The payment: the bank against the same personal account.
    let paid = rows.iter().find(|r| r[6] == "1200").expect("a row on 1200");
    assert_eq!(paid[0], "119,00");
    assert_eq!(paid[1], "\"S\"");
    assert_eq!(paid[7], "10000");
    // The file is in Windows-1252.
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(file["data"].as_str().unwrap())
        .unwrap();
    assert!(
        bytes.windows(4).any(|w| w == b"M\xe4rz"),
        "not Windows-1252"
    );
    assert_eq!(
        file["filename"],
        "EXTF_Buchungsstapel_20260301_20260331.csv"
    );

    // Not an automatic account: the key goes with the gross amount.
    t.create(
        "datev_account_setting",
        json!({ "company_id": c, "account_id": erloese, "automatic": false }),
    )
    .await;
    let file = run(
        &t,
        "datev.export_postings",
        json!({ "company_id": c, "from": "2026-03-01", "to": "2026-03-15" }),
    )
    .await;
    let all = lines(&file);
    let sale: Vec<&str> = all[2].split(';').collect();
    assert_eq!(
        (sale[0], sale[6], sale[8]),
        ("119,00", "8400", "\"3\""),
        "{all:?}"
    );

    // One fiscal year per batch.
    let (status, body) = t
        .command(
            &t.admin,
            "datev.export_postings",
            json!({ "company_id": c, "from": "2026-12-01", "to": "2027-01-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // The customer as a Debitor, on the same account.
    let parties = run(&t, "datev.export_parties", json!({ "company_id": c })).await;
    let all = lines(&parties);
    let header: Vec<&str> = all[0].split(';').collect();
    assert_eq!(
        header[..5],
        ["\"EXTF\"", "700", "16", "\"Debitoren/Kreditoren\"", "5"]
    );
    assert_eq!(all[1].split(';').count(), 254);
    let row: Vec<&str> = all[2].split(';').collect();
    assert_eq!(row[0], "10000");
    assert_eq!(row[1], "\"Müller GmbH\"");
    assert_eq!(row[8], "\"DE\"");
    assert_eq!(row[9], "\"123456789\"");
    assert_eq!(row[18], "\"München\"");

    // Each export is on record.
    let (_, log) = t
        .get(
            &t.admin,
            &format!("/api/entities/export_log?company_id={c}"),
        )
        .await;
    assert_eq!(log["total"], 3, "{log}");
    t.stop().await;
}

#[tokio::test]
async fn a_us_company_reports_its_1099_vendors_and_a_canadian_one_its_t5018() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let bank = created["roles"]["bank"]["id"].clone();
    let expense = account(&t, &c, "6070").await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "company", "party_id": c,
                "tax_ids": [{ "scheme": "us_ein", "value": "12-3456789" }] }),
    )
    .await;
    let mut vendors = vec![];
    for (code, amount, ssn) in [
        ("JANE", "2500.00", "123-45-6789"),
        ("BOB", "1500.00", "987-65-4321"),
    ] {
        let v = t
            .create(
                "vendor",
                json!({ "code": code, "name": format!("{code} Doe") }),
            )
            .await;
        t.create(
            "vendor_information_return",
            json!({ "vendor_id": v, "form": "us_1099", "box": "nec_1" }),
        )
        .await;
        run(
            &t,
            "party.set",
            json!({ "party_type": "vendor", "party_id": v,
                    "addresses": [{ "kind": "billing", "is_default": true, "street": "1 Main St",
                                    "city": "Columbus", "region": "OH", "postcode": "43004",
                                    "country_code": "US" }],
                    "tax_ids": [{ "scheme": "us_ssn", "value": ssn }] }),
        )
        .await;
        let bill = invoice(
            &t,
            "vendor_invoice",
            &c,
            ("vendor_id", &v),
            "2026-02-10",
            json!({ "account_id": expense, "description": "Consulting", "qty": "1",
                    "unit_price": amount }),
        )
        .await;
        pay(&t, &c, &bank, &bill, "2026-03-01").await;
        vendors.push(v);
    }
    // An SSN is nine digits.
    let (status, _) = t
        .command(
            &t.admin,
            "party.set",
            json!({ "party_type": "vendor", "party_id": vendors[0],
                    "tax_ids": [{ "scheme": "us_ssn", "value": "12345" }] }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);

    let report = run(
        &t,
        "information_return.report",
        json!({ "company_id": c, "form": "us_1099", "year": 2026 }),
    )
    .await;
    let jane = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["vendor_code"] == "JANE")
        .unwrap();
    assert_eq!(
        jane["amount"].as_str().unwrap().parse::<f64>().unwrap(),
        2500.0
    );
    assert_eq!(
        jane["threshold"].as_str().unwrap().parse::<f64>().unwrap(),
        2000.0
    );
    assert_eq!(jane["reportable"], true);
    assert_eq!(jane["tin"], "123-45-6789");
    assert_eq!(report["reportable"], 1, "{report}");
    // In 2025 the threshold was $600: both would be reported.
    let earlier = run(
        &t,
        "information_return.report",
        json!({ "company_id": c, "form": "us_1099", "year": 2025,
                "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert_eq!(earlier["reportable"], 2, "{earlier}");
    // A clerk sees the TIN masked, and may not export.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["payment:read", "vendor:read"]),
            Some(&c),
        )
        .await;
    let (status, seen) = t
        .command(
            &clerk,
            "information_return.report",
            json!({ "company_id": c, "form": "us_1099", "year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert!(seen.to_string().contains("***6789"), "{seen}");
    assert!(!seen.to_string().contains("123-45-6789"), "{seen}");
    let (status, _) = t
        .command(
            &clerk,
            "information_return.export",
            json!({ "company_id": c, "form": "us_1099", "year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let file = run(
        &t,
        "information_return.export",
        json!({ "company_id": c, "form": "us_1099", "year": 2026 }),
    )
    .await;
    let all = lines(&file);
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all[0].starts_with("tax_year,form,payer_tin"));
    assert!(
        all[1].starts_with("2026,1099-NEC,12-3456789,"),
        "{}",
        all[1]
    );
    assert!(all[1].contains(",SSN,123-45-6789,JANE Doe,"), "{}", all[1]);
    assert!(all[1].contains(",2500.00,"), "{}", all[1]);

    // Canada: a subcontractor paid $600 is reported on a T5018.
    let ca = run(
        &t,
        "company.setup",
        json!({ "template": "ca_small_business", "code": "QC", "name": "Construction QC",
                "first_year": 2026 }),
    )
    .await;
    let cc = ca["company"]["id"].as_str().unwrap().to_owned();
    let sub = t
        .create("vendor", json!({ "code": "SUB", "name": "Sub Inc" }))
        .await;
    t.create(
        "vendor_information_return",
        json!({ "vendor_id": sub, "form": "ca_t5018" }),
    )
    .await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "vendor", "party_id": sub,
                "tax_ids": [{ "scheme": "ca_bn", "value": "123456789RT0001" }] }),
    )
    .await;
    let cost = account(&t, &cc, "6060").await;
    let bill = invoice(
        &t,
        "vendor_invoice",
        &cc,
        ("vendor_id", &sub),
        "2026-04-10",
        json!({ "account_id": cost, "description": "Framing", "qty": "1", "unit_price": "600.00" }),
    )
    .await;
    pay(&t, &cc, &ca["roles"]["bank"]["id"], &bill, "2026-04-20").await;
    let t5018 = run(
        &t,
        "information_return.report",
        json!({ "company_id": cc, "form": "ca_t5018", "year": 2026 }),
    )
    .await;
    assert_eq!(t5018["reportable"], 1, "{t5018}");
    assert_eq!(t5018["rows"][0]["tin_type"], "BN");
    assert!(
        t5018["rows"][0]["missing"]
            .as_str()
            .unwrap()
            .contains("address")
    );
    t.stop().await;
}

#[tokio::test]
async fn the_general_ledger_exports_as_rows_and_as_csv() {
    let t = TestApp::start().await;
    let created = run(
        &t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026 }),
    )
    .await;
    let c = created["company"]["id"].as_str().unwrap().to_owned();
    let site = t
        .create(
            "dimension",
            json!({ "company_id": c, "code": "SITE", "name": "Site" }),
        )
        .await;
    let ohio = t
        .create(
            "dimension_value",
            json!({ "company_id": c, "dimension_id": site, "code": "OHIO", "name": "Ohio" }),
        )
        .await;
    t.create(
        "dimension_rule",
        json!({ "company_id": c, "value_id": ohio }),
    )
    .await;
    let customer = t
        .create("customer", json!({ "code": "ACME", "name": "Acme, Inc." }))
        .await;
    invoice(
        &t,
        "sales_invoice",
        &c,
        ("customer_id", &customer),
        "2026-05-02",
        json!({ "account_id": created["roles"]["revenue"]["id"], "description": "Service",
                "qty": "1", "unit_price": "250.00" }),
    )
    .await;
    let rows = run(
        &t,
        "gl.export",
        json!({ "company_id": c, "from": "2026-05-01", "to": "2026-05-31" }),
    )
    .await;
    assert_eq!(rows["total"], 2, "{rows}");
    let receivable = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["party_code"] == "ACME")
        .expect("the customer's entry");
    assert_eq!(
        receivable["debit"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        250.0
    );
    assert_eq!(receivable["dimensions"], "SITE:OHIO");
    let csv = run(
        &t,
        "gl.export",
        json!({ "company_id": c, "from": "2026-05-01", "to": "2026-05-31", "format": "csv" }),
    )
    .await;
    let file = &csv["file"];
    let all = lines(file);
    assert!(
        all[0].starts_with("\u{feff}posting_date,doc_type,"),
        "{}",
        all[0]
    );
    assert!(all[0].ends_with(",dimension_SITE"), "{}", all[0]);
    assert_eq!(all.len(), 3);
    assert!(
        all.iter().any(|l| l.contains(",\"Acme, Inc.\",OHIO")),
        "{all:?}"
    );
    assert!(
        all[1].starts_with("2026-05-02,sales_invoice,"),
        "{}",
        all[1]
    );
    t.stop().await;
}
