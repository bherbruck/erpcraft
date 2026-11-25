//! Review 8: bank files of payment runs.

mod common;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

struct Us {
    company: String,
    bank: String,
    services: String,
    vendor: String,
}

async fn setup(t: &TestApp) -> Us {
    let (status, body) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_small_business", "code": "OHIO", "name": "OHIO Ltd",
                    "first_year": 2026, "years": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let company = body["company"]["id"].as_str().unwrap().to_owned();
    let bank = body["roles"]["bank"]["id"].as_str().unwrap().to_owned();
    let services = t
        .create(
            "account",
            json!({ "company_id": company, "code": "6990", "name": "Services",
                    "account_type": "expense" }),
        )
        .await;
    t.create(
        "bank_account_number",
        json!({ "company_id": company, "bank_account_id": bank, "account_number": "987654321",
                "routing_number": "021000021", "ach_company_id": "1311234567" }),
    )
    .await;
    let vendor = t
        .create("vendor", json!({ "code": "PARTS", "name": "Parts Supply" }))
        .await;
    t.create(
        "party_bank_account",
        json!({ "party_type": "vendor", "party_id": vendor, "routing_number": "011000015",
                "account_number": "44556677", "account_type": "checking", "is_default": true }),
    )
    .await;
    Us {
        company,
        bank,
        services,
        vendor,
    }
}

async fn bill(t: &TestApp, us: &Us, amount: &str) {
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({ "company_id": us.company, "doc_date": "2026-03-01", "body": {
                "vendor_id": us.vendor, "vendor_reference": "P-1",
                "lines": [{ "account_id": us.services, "description": "Service", "qty": "1",
                            "unit_price": amount }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(t, "vendor_invoice.post", json!({ "id": id(&draft) })).await;
}

async fn posted_run(t: &TestApp, us: &Us) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/payment_run",
            json!({ "company_id": us.company, "doc_date": "2026-03-15", "body": {
                "bank_account_id": us.bank, "due_by": "2026-03-15" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(t, "payment_run.propose", json!({ "id": id(&draft) })).await;
    run(t, "payment_run.approve", json!({ "id": id(&draft) })).await;
    run(t, "payment_run.post", json!({ "id": id(&draft) })).await
}

/// A payment of the run reversed afterwards is still in the run's bank
/// file: the bank pays a vendor the books say was not paid.
#[tokio::test]
async fn a_reversed_payment_is_not_in_the_bank_file() {
    let t = TestApp::start().await;
    let us = setup(&t).await;
    bill(&t, &us, "100.00").await;
    let posted = posted_run(&t, &us).await;
    let payment = posted["body"]["lines"][0]["payment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    run(&t, "payment.reverse", json!({ "id": payment })).await;
    let (status, file) = t
        .command(
            &t.admin,
            "payment_run.bank_file",
            json!({ "id": id(&posted), "format": "nacha" }),
        )
        .await;
    assert!(
        status != StatusCode::OK || file["count"] == json!(0),
        "the reversed payment is still paid by the file: {file}"
    );
    t.stop().await;
}

/// An amount of $100,000,000.00 or more does not fit the 10-digit amount
/// of a NACHA entry; it is cut to its last ten digits instead of refused.
#[tokio::test]
async fn a_nacha_amount_too_large_is_refused_not_cut() {
    let t = TestApp::start().await;
    let us = setup(&t).await;
    bill(&t, &us, "123456789.00").await;
    let posted = posted_run(&t, &us).await;
    let (status, file) = t
        .command(
            &t.admin,
            "payment_run.bank_file",
            json!({ "id": id(&posted), "format": "nacha" }),
        )
        .await;
    if status == StatusCode::OK {
        let content = file["content"].as_str().unwrap();
        let entry = content.lines().find(|r| r.starts_with('6')).unwrap();
        panic!(
            "the entry's amount 12345678900 cents is cut to {}: {entry}",
            &entry[29..39]
        );
    }
    t.stop().await;
}

/// A user may read only some vendors' bank accounts: the bank file, which
/// shows them, is refused rather than shown in full.
#[tokio::test]
async fn a_bank_file_reads_vendors_accounts_by_the_callers_own_rules() {
    let t = TestApp::start().await;
    let us = setup(&t).await;
    bill(&t, &us, "100.00").await;
    let posted = posted_run(&t, &us).await;
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["payment_run:read",
                   { "allow": "party_bank_account:read",
                     "where": "record.account_type == 'savings'" }]),
            Some(&us.company),
        )
        .await;
    let (status, body) = t
        .command(
            &clerk,
            "payment_run.bank_file",
            json!({ "id": id(&posted), "format": "nacha" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    t.stop().await;
}

/// Written again without a reversed payment only when asked, and the
/// remittance advice is not sent for it.
#[tokio::test]
async fn a_run_with_a_reversed_payment_is_written_again_only_when_asked() {
    let t = TestApp::start().await;
    let us = setup(&t).await;
    bill(&t, &us, "100.00").await;
    let other = t
        .create("vendor", json!({ "code": "TOOLS", "name": "Tool Supply" }))
        .await;
    t.create(
        "party_bank_account",
        json!({ "party_type": "vendor", "party_id": other, "routing_number": "011000015",
                "account_number": "Ab-99", "account_type": "savings", "is_default": true }),
    )
    .await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({ "company_id": us.company, "doc_date": "2026-03-01", "body": {
                "vendor_id": other, "vendor_reference": "T-1",
                "lines": [{ "account_id": us.services, "description": "Tools", "qty": "1",
                            "unit_price": "40.00" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(&t, "vendor_invoice.post", json!({ "id": id(&draft) })).await;
    let posted = posted_run(&t, &us).await;
    let parts_payment = posted["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["vendor_id"] == json!(us.vendor))
        .unwrap()["payment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    run(&t, "payment.reverse", json!({ "id": parts_payment })).await;
    let (status, body) = t
        .command(
            &t.admin,
            "payment_run.bank_file",
            json!({ "id": id(&posted), "format": "nacha" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let file = run(
        &t,
        "payment_run.bank_file",
        json!({ "id": id(&posted), "format": "nacha", "without_reversed": true }),
    )
    .await;
    assert_eq!(file["count"], 1, "{file}");
    let content = file["content"].as_str().unwrap();
    let entry = content.lines().find(|r| r.starts_with('6')).unwrap();
    // Savings, and the account number as the vendor gave it.
    assert_eq!(&entry[1..3], "32");
    assert_eq!(&entry[12..29], "Ab-99            ");
    t.stop().await;
}
