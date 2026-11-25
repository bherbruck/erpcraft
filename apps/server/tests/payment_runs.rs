//! Payment runs: what is due to vendors proposed, approved, paid through
//! ordinary payments, written as bank files and advised by mail.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

struct Side {
    id: String,
    bank: String,
    payable: String,
}

async fn company(t: &TestApp, template: &str, code: &str) -> Side {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": template, "code": code, "name": format!("{code} Ltd"),
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Side {
        id: body["company"]["id"].as_str().unwrap().to_owned(),
        bank: body["roles"]["bank"]["id"].as_str().unwrap().to_owned(),
        payable: body["roles"]["payable"]["id"].as_str().unwrap().to_owned(),
    }
}

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn expense(t: &TestApp, company: &str, code: &str) -> String {
    t.create(
        "account",
        json!({ "company_id": company, "code": code, "name": "Services", "account_type": "expense" }),
    )
    .await
}

/// A posted vendor invoice of `amount` for a service.
async fn bill(
    t: &TestApp,
    (company, account): (&str, &str),
    vendor: &str,
    date: &str,
    amount: &str,
    reference: &str,
    terms: Option<&str>,
) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({ "company_id": company, "doc_date": date, "body": {
                "vendor_id": vendor, "vendor_reference": reference, "payment_terms_id": terms,
                "lines": [{ "account_id": account, "description": "Service", "qty": "1",
                            "unit_price": amount }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(t, "vendor_invoice.post", json!({ "id": id(&draft) })).await
}

async fn new_run(t: &TestApp, company: &str, bank: &str) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/payment_run",
            json!({ "company_id": company, "doc_date": "2026-03-15", "body": {
                "bank_account_id": bank, "due_by": "2026-03-15" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    draft
}

#[tokio::test]
async fn vendors_are_paid_in_a_run_and_the_bank_gets_a_nacha_file() {
    // Remittance advice goes to an SMTP server on this host, which a server
    // sends to only when it allows private addresses.
    // SAFETY: set before this test's server starts; only the mail module
    // reads it, as a server starts.
    unsafe { std::env::set_var(erpcraft_mail::ALLOW_PRIVATE, "1") };
    let t = TestApp::start().await;
    let us = company(&t, "us_small_business", "OHIO").await;
    let services = expense(&t, &us.id, "6990").await;
    // The bank account's numbers; a routing number must have its check digit.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/bank_account_number",
            json!({ "company_id": us.id, "bank_account_id": us.bank, "account_number": "987654321",
                    "routing_number": "021000022", "ach_company_id": "1311234567" }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED);
    t.create(
        "bank_account_number",
        json!({ "company_id": us.id, "bank_account_id": us.bank, "account_number": "987654321",
                "routing_number": "021000021", "ach_company_id": "1311234567" }),
    )
    .await;
    let vendor = |code: &'static str| {
        let t = &t;
        async move {
            t.create(
                "vendor",
                json!({ "code": code, "name": format!("{code} Supply") }),
            )
            .await
        }
    };
    let parts = vendor("PARTS").await;
    let held = vendor("HELD").await;
    let later = vendor("LATER").await;
    t.create(
        "party_bank_account",
        json!({ "party_type": "vendor", "party_id": parts, "routing_number": "011000015",
                "account_number": "44556677", "account_type": "checking", "is_default": true }),
    )
    .await;
    let two_ten = t
        .create(
            "payment_terms",
            json!({ "code": "2-10-N30", "name": "2% 10, net 30", "net_days": 30,
                    "discount_percent": "2", "discount_days": 10 }),
        )
        .await;
    let net30 = t
        .create(
            "payment_terms",
            json!({ "code": "N30", "name": "Net 30", "net_days": 30 }),
        )
        .await;
    // Due; not due but with a discount still open; held; not due.
    bill(
        &t,
        (&us.id, &services),
        &parts,
        "2026-03-01",
        "100.00",
        "P-100",
        None,
    )
    .await;
    bill(
        &t,
        (&us.id, &services),
        &parts,
        "2026-03-10",
        "200.00",
        "P-200",
        Some(&two_ten),
    )
    .await;
    bill(
        &t,
        (&us.id, &services),
        &held,
        "2026-03-01",
        "50.00",
        "H-1",
        None,
    )
    .await;
    bill(
        &t,
        (&us.id, &services),
        &later,
        "2026-03-10",
        "70.00",
        "L-1",
        Some(&net30),
    )
    .await;
    t.create(
        "payment_hold",
        json!({ "company_id": us.id, "vendor_id": held, "reason": "disputed delivery" }),
    )
    .await;
    // Money paid to PARTS on account earlier: a credit the run nets.
    let (status, prepaid) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({ "company_id": us.id, "doc_date": "2026-03-02", "body": {
                "direction": "outgoing", "bank_account_id": us.bank, "on_account": "6.00",
                "party_type": "vendor", "party_id": parts, "account_id": us.payable } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{prepaid}");
    run(&t, "payment.post", json!({ "id": id(&prepaid) })).await;

    let draft = new_run(&t, &us.id, &us.bank).await;
    let proposed = run(&t, "payment_run.propose", json!({ "id": id(&draft) })).await;
    let lines = proposed["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3, "{proposed}");
    assert!(lines.iter().all(|l| l["vendor_id"] == json!(parts)));
    let discounted = lines.iter().find(|l| dec(&l["discount"]) > 0.0).unwrap();
    assert_eq!(dec(&discounted["discount"]), 4.0);
    assert_eq!(dec(&discounted["amount"]), 196.0);
    assert!(
        lines.iter().any(|l| dec(&l["amount"]) == -6.0),
        "{proposed}"
    );
    assert_eq!(dec(&proposed["summary"]["total"]), 290.0);

    // An item put on hold after it was proposed stops the run until the
    // hold is lifted.
    let due = lines.iter().find(|l| dec(&l["amount"]) == 100.0).unwrap();
    let hold = t
        .create(
            "payment_hold",
            json!({ "company_id": us.id, "open_item_id": due["open_item_id"],
                    "reason": "price query" }),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "payment_run.approve", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("price query")
    );
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/payment_hold/{hold}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // The company's policy decides approval.
    let rule = t
        .create(
            "policy_rule",
            json!({ "company_id": us.id, "point": "payment_run.approve", "effect": "refuse",
                    "condition": "facts.total > 250 && facts.count >= 1",
                    "message": "runs over 250 need the controller" }),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "payment_run.approve", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("controller")
    );
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{rule}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    run(&t, "payment_run.approve", json!({ "id": id(&draft) })).await;
    let posted = run(&t, "payment_run.post", json!({ "id": id(&draft) })).await;
    assert_eq!(posted["status"], "posted");
    let payment = posted["body"]["lines"][0]["payment_id"].clone();
    assert!(payment.is_string(), "{posted}");
    let (_, paid) = t
        .get(
            &t.admin,
            &format!("/api/documents/payment/{}", payment.as_str().unwrap()),
        )
        .await;
    assert_eq!(paid["status"], "posted");
    assert_eq!(dec(&paid["summary"]["total"]), 290.0);
    // A posted item is not proposed again.
    let again = new_run(&t, &us.id, &us.bank).await;
    let (_, again) = t
        .command(&t.admin, "payment_run.propose", json!({ "id": id(&again) }))
        .await;
    assert_eq!(again["body"]["lines"], json!([]));

    // The NACHA file: 94-character records in blocks of ten, one credit
    // with an addenda carrying the vendor's references.
    let file = run(
        &t,
        "payment_run.bank_file",
        json!({ "id": id(&draft), "format": "nacha" }),
    )
    .await;
    let content = file["content"].as_str().unwrap();
    let records: Vec<&str> = content.lines().collect();
    assert_eq!(records.len() % 10, 0);
    assert!(records.iter().all(|r| r.len() == 94), "{content}");
    assert!(records[0].starts_with("101 021000021"));
    let batch = records[1];
    assert_eq!(&batch[50..53], "CCD");
    let entry = records.iter().find(|r| r.starts_with('6')).unwrap();
    assert_eq!(&entry[1..3], "22");
    assert_eq!(&entry[3..12], "011000015");
    assert_eq!(&entry[29..39], "0000029000");
    let addenda = records.iter().find(|r| r.starts_with('7')).unwrap();
    assert!(
        addenda.contains("P-100") && addenda.contains("P-200"),
        "{addenda}"
    );
    let control = records.iter().find(|r| r.starts_with('9')).unwrap();
    assert_eq!(&control[13..21], "00000002");
    assert_eq!(&control[21..31], "0001100001");
    assert_eq!(&control[43..55], "000000029000");
    // Not in euros: no SEPA file.
    let (status, _) = t
        .command(
            &t.admin,
            "payment_run.bank_file",
            json!({ "id": id(&draft), "format": "sepa" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Remittance advice: by mail to the vendor's contact.
    run(
        &t,
        "smtp_settings.set",
        json!({ "company_id": us.id, "host": "127.0.0.1", "port": 2525, "security": "none",
                "from_address": "ap@ohio.example" }),
    )
    .await;
    run(
        &t,
        "party.set",
        json!({ "party_type": "vendor", "party_id": parts, "contacts": [
            { "name": "Receivables", "email": "ar@parts.example", "is_default": true }] }),
    )
    .await;
    let sent = run(
        &t,
        "payment_run.send_remittances",
        json!({ "id": id(&draft) }),
    )
    .await;
    assert_eq!(sent["sent"].as_array().unwrap().len(), 1, "{sent}");
    assert!(sent["sent"][0]["message_id"].is_string(), "{sent}");
    t.stop().await;
}

#[tokio::test]
async fn a_sepa_run_writes_pain_001() {
    let t = TestApp::start().await;
    let de = company(&t, "de_skr03", "MUSTER").await;
    let services = expense(&t, &de.id, "4900").await;
    t.create(
        "bank_account_number",
        json!({ "company_id": de.id, "bank_account_id": de.bank,
                "iban": "DE89370400440532013000", "bic": "COBADEFFXXX" }),
    )
    .await;
    let vendor = t
        .create(
            "vendor",
            json!({ "code": "FOURN", "name": "Fournisseur & Fils" }),
        )
        .await;
    // An IBAN with wrong check digits is refused.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/party_bank_account",
            json!({ "party_type": "vendor", "party_id": vendor,
                    "iban": "FR1520041010050500013M02606" }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED);
    t.create(
        "party_bank_account",
        json!({ "party_type": "vendor", "party_id": vendor, "iban": "FR1420041010050500013M02606",
                "bic": "PSSTFRPPXXX", "is_default": true }),
    )
    .await;
    bill(
        &t,
        (&de.id, &services),
        &vendor,
        "2026-03-01",
        "1200.00",
        "F-2026-17",
        None,
    )
    .await;
    let draft = new_run(&t, &de.id, &de.bank).await;
    run(&t, "payment_run.propose", json!({ "id": id(&draft) })).await;
    run(&t, "payment_run.approve", json!({ "id": id(&draft) })).await;
    run(&t, "payment_run.post", json!({ "id": id(&draft) })).await;
    let file = run(
        &t,
        "payment_run.bank_file",
        json!({ "id": id(&draft), "format": "sepa" }),
    )
    .await;
    let xml = file["content"].as_str().unwrap();
    assert!(xml.contains("urn:iso:std:iso:20022:tech:xsd:pain.001.001.09"));
    assert!(xml.contains("<IBAN>FR1420041010050500013M02606</IBAN>"));
    assert!(xml.contains("<InstdAmt Ccy=\"EUR\">1200.00</InstdAmt>"));
    assert!(xml.contains("<Nm>Fournisseur &amp; Fils</Nm>"));
    assert!(xml.contains("<Ustrd>F-2026-17</Ustrd>"));
    assert_eq!(dec(&file["total"]), 1200.0);
    // Against the official schema, where it is at hand (PAIN001_XSD, the
    // pain.001.001.09.xsd of iso20022.org) and xmllint is installed.
    if let Ok(xsd) = std::env::var("PAIN001_XSD") {
        let dir = std::env::temp_dir().join(format!("pain001-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.xml");
        std::fs::write(&path, xml).unwrap();
        let out = std::process::Command::new("xmllint")
            .args(["--noout", "--schema", &xsd])
            .arg(&path)
            .output()
            .expect("xmllint");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
    t.stop().await;
}
