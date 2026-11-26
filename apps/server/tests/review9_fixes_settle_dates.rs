//! Review 9 of the review-8 fixes: an item is settled no earlier than its
//! own date (open items migration 11), so that "the ledger and the open
//! items as of a day agree". The trigger lets every reversal through on the
//! premise that "a reversal ... is dated no earlier than what it reverses
//! anyway" -- which nothing enforces -- and the proposals that feed
//! payments (payment runs, bank lines) still offer items the post refuses.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

struct Shop {
    company: String,
    revenue: String,
    bank: String,
    services: String,
}

async fn shop(t: &TestApp) -> Shop {
    let created = run(
        t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026, "years": 1 }),
    )
    .await;
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let services = t
        .create(
            "account",
            json!({ "company_id": company, "code": "6990", "name": "Services",
                    "account_type": "expense" }),
        )
        .await;
    Shop {
        revenue: created["roles"]["revenue"]["id"]
            .as_str()
            .unwrap()
            .to_owned(),
        bank: created["roles"]["bank"]["id"].as_str().unwrap().to_owned(),
        services,
        company,
    }
}

impl Shop {
    async fn invoice(&self, t: &TestApp, customer: &str, date: &str, amount: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/sales_invoice",
                json!({ "company_id": self.company, "doc_date": date, "body": {
                    "customer_id": customer,
                    "lines": [{ "account_id": self.revenue, "description": "Widgets",
                                "qty": "1", "unit_price": amount }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "sales_invoice.post", json!({ "id": id(&draft) })).await
    }

    async fn item_of(&self, t: &TestApp, kind: &str, party_type: &str, party: &str) -> String {
        let list = run(
            t,
            "open_item.list",
            json!({ "company_id": self.company, "kind": kind, "party_type": party_type,
                    "party_id": party, "include_settled": true }),
        )
        .await;
        list["items"][0]["id"].as_str().unwrap().to_owned()
    }

    async fn pay(&self, t: &TestApp, date: &str, item: &str, amount: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/payment",
                json!({ "company_id": self.company, "doc_date": date, "body": {
                    "direction": "incoming", "bank_account_id": self.bank,
                    "allocations": [{ "open_item_id": item, "amount": amount }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "payment.post", json!({ "id": id(&draft) })).await
    }

    /// The control accounts' balances beside what the open items add up to,
    /// as of a day: the agreement the new rule is there to keep.
    async fn disagreement(&self, t: &TestApp, kind: &str, as_of: &str) -> Option<String> {
        let aging = run(
            t,
            "open_item.aging",
            json!({ "company_id": self.company, "kind": kind, "as_of": as_of }),
        )
        .await;
        for account in aging["accounts"].as_array().unwrap() {
            if dec(&account["balance"]) != dec(&account["open"]) {
                return Some(format!(
                    "as of {as_of}, account {} holds {} but its open items add up to {}",
                    account["code"], account["balance"], account["open"]
                ));
            }
        }
        None
    }
}

/// Reversing a payment with a posting date before the invoice it paid
/// writes a settlement reversal dated before the item exists: the trigger
/// waves it through (`reverses_id IS NOT NULL`) and reverse_posting accepts
/// any date. As of a day between the two, the receivable holds the
/// reversal but no open item stands behind it.
#[tokio::test]
async fn a_payment_reversal_dated_before_the_invoice_breaks_the_agreement() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    let acme = t
        .create("customer", json!({ "code": "ACME", "name": "Acme Co" }))
        .await;
    shop.invoice(&t, &acme, "2026-03-10", "100.00").await;
    let item = shop.item_of(&t, "receivable", "customer", &acme).await;
    let paid = shop.pay(&t, "2026-03-15", &item, "100.00").await;
    let (status, reversed) = t
        .command(
            &t.admin,
            "payment.reverse",
            json!({ "id": id(&paid), "posting_date": "2026-03-01" }),
        )
        .await;
    if status == StatusCode::OK {
        let broken = shop.disagreement(&t, "receivable", "2026-03-05").await;
        assert!(
            broken.is_none(),
            "a payment reversal dated 2026-03-01, before the invoice of 2026-03-10 it paid, \
             was accepted: {}",
            broken.unwrap()
        );
    } else {
        eprintln!("refused: {reversed}");
    }
    t.stop().await;
}

/// The same through the invoice: reversing it with a posting date before
/// its own calls its item off (`cancels`) on a day the item did not exist.
#[tokio::test]
async fn an_invoice_reversal_dated_before_the_invoice_breaks_the_agreement() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    let acme = t
        .create("customer", json!({ "code": "ACME", "name": "Acme Co" }))
        .await;
    let invoice = shop.invoice(&t, &acme, "2026-03-10", "100.00").await;
    let (status, reversed) = t
        .command(
            &t.admin,
            "sales_invoice.reverse",
            json!({ "id": id(&invoice), "posting_date": "2026-03-01" }),
        )
        .await;
    if status == StatusCode::OK {
        let broken = shop.disagreement(&t, "receivable", "2026-03-05").await;
        assert!(
            broken.is_none(),
            "an invoice of 2026-03-10 reversed as of 2026-03-01 was accepted: {}",
            broken.unwrap()
        );
    } else {
        eprintln!("refused: {reversed}");
    }
    t.stop().await;
}

/// A payment run dated before a vendor invoice that is already due by the
/// run's due_by proposes the invoice; approving accepts it; posting is then
/// refused by the open-items trigger for the whole run (all or nothing),
/// with a database message that names no line. The proposal should leave
/// out what the run's payments cannot settle.
#[tokio::test]
async fn a_payment_run_proposes_an_invoice_dated_after_the_run_and_cannot_post() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    let vendor = t
        .create("vendor", json!({ "code": "PARTS", "name": "Parts Supply" }))
        .await;
    for (date, amount) in [("2026-03-01", "100.00"), ("2026-03-20", "50.00")] {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/vendor_invoice",
                json!({ "company_id": shop.company, "doc_date": date, "body": {
                    "vendor_id": vendor, "vendor_reference": format!("P-{date}"),
                    "lines": [{ "account_id": shop.services, "description": "Service",
                                "qty": "1", "unit_price": amount }] } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(&t, "vendor_invoice.post", json!({ "id": id(&draft) })).await;
    }
    // A run paid on 2026-03-15, for everything due by the end of the month.
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/payment_run",
            json!({ "company_id": shop.company, "doc_date": "2026-03-15", "body": {
                "bank_account_id": shop.bank, "due_by": "2026-03-31" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let proposed = run(&t, "payment_run.propose", json!({ "id": id(&draft) })).await;
    let lines = proposed["body"]["lines"].as_array().unwrap().len();
    run(&t, "payment_run.approve", json!({ "id": id(&draft) })).await;
    let (status, posted) = t
        .command(&t.admin, "payment_run.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a run of {lines} proposed line(s), dated 2026-03-15, proposed the invoice of \
         2026-03-20 and then cannot post at all: {posted}"
    );
    t.stop().await;
}

/// A bank line dated before the invoice it is matched to: bank_statement
/// match drafts the payment at the line's date, which payment.post refuses.
/// The match (and the reconciliation's suggestion) should not offer an item
/// dated after the line, or the payment should be on account.
#[tokio::test]
async fn a_bank_line_matched_to_a_later_invoice_drafts_a_payment_that_cannot_post() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    let acme = t
        .create("customer", json!({ "code": "ACME", "name": "Acme Co" }))
        .await;
    shop.invoice(&t, &acme, "2026-03-10", "100.00").await;
    let item = shop.item_of(&t, "receivable", "customer", &acme).await;
    let (status, statement) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            json!({ "company_id": shop.company, "doc_date": "2026-03-31", "body": {
                "bank_account_id": shop.bank, "period_start": "2026-03-01",
                "period_end": "2026-03-31", "opening_balance": "0", "closing_balance": "100.00",
                "lines": [{ "booking_date": "2026-03-05", "amount": "100.00",
                            "counterparty_name": "Acme Co", "reference": "prepayment" }] } }),
        )
        .await;
    if status != StatusCode::CREATED {
        panic!("could not draft the statement (test set-up): {statement}");
    }
    let line = statement["body"]["lines"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, matched) = t
        .command(
            &t.admin,
            "bank_statement.match",
            json!({ "id": id(&statement), "line_id": line, "action": "open_items",
                    "open_item_ids": [item] }),
        )
        .await;
    if status != StatusCode::OK {
        eprintln!("match refused: {matched}");
        t.stop().await;
        return;
    }
    let payment = matched["body"]["lines"][0]["payment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, posted) = t
        .command(&t.admin, "payment.post", json!({ "id": payment }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the bank line of 2026-03-05 was matched to the invoice of 2026-03-10; the payment it \
         drafted cannot post: {posted}"
    );
    t.stop().await;
}
