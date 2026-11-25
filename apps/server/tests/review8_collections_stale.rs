//! Review 8: dunning runs post what was worked out when the draft was saved,
//! and statements that do not add up.

mod common;

use std::collections::BTreeMap;

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
    accounts: BTreeMap<String, String>,
}

async fn shop(t: &TestApp) -> Shop {
    let created = run(
        t,
        "company.setup",
        json!({ "template": "us_small_business", "code": "OHIO", "name": "Ohio Widgets",
                "first_year": 2026 }),
    )
    .await;
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let (_, list) = t
        .get(
            &t.admin,
            &format!("/api/entities/account?company_id={company}&limit=500"),
        )
        .await;
    Shop {
        revenue: created["roles"]["revenue"]["id"]
            .as_str()
            .unwrap()
            .to_owned(),
        bank: created["roles"]["bank"]["id"].as_str().unwrap().to_owned(),
        accounts: list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["code"].as_str().unwrap().to_owned(),
                    a["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect(),
        company,
    }
}

impl Shop {
    async fn customer(&self, t: &TestApp, code: &str) -> String {
        t.create(
            "customer",
            json!({ "code": code, "name": format!("{code} Co") }),
        )
        .await
    }

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

    async fn item_of(&self, t: &TestApp, customer: &str, document: &Value) -> String {
        let list = run(
            t,
            "open_item.list",
            json!({ "company_id": self.company, "kind": "receivable", "party_type": "customer",
                    "party_id": customer, "include_settled": true }),
        )
        .await;
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["document_id"] == document["id"])
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn pay(&self, t: &TestApp, date: &str, item: &str, amount: &str) -> (StatusCode, Value) {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/payment",
                json!({ "company_id": self.company, "doc_date": date, "body": {
                    "direction": "incoming", "bank_account_id": self.bank,
                    "allocations": [{ "open_item_id": item, "amount": amount }] } }),
            )
            .await;
        if status != StatusCode::CREATED {
            return (status, draft);
        }
        t.command(&t.admin, "payment.post", json!({ "id": id(&draft) }))
            .await
    }

    async fn draft(&self, t: &TestApp, doc_type: &str, date: &str, body: Value) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                &format!("/api/documents/{doc_type}"),
                json!({ "company_id": self.company, "doc_date": date, "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        draft
    }

    async fn levels(&self, t: &TestApp) {
        let other_income = &self.accounts["4900"];
        for (level, days, fee, rate) in [(1, 7, "10.00", "12"), (2, 30, "25.00", "12")] {
            t.create(
                "dunning_level",
                json!({ "company_id": self.company, "level": level,
                        "name": format!("Reminder {level}"), "days_overdue": days, "fee": fee,
                        "fee_account_id": other_income, "interest_rate": rate,
                        "interest_account_id": other_income, "pay_within_days": 10 }),
            )
            .await;
        }
    }
}

/// An invoice paid in full after the run was proposed is still dunned:
/// posting charges a fee and interest on an item that owes nothing.
#[tokio::test]
async fn a_run_does_not_dun_an_item_paid_after_it_was_proposed() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    shop.levels(&t).await;
    let acme = shop.customer(&t, "ACME").await;
    let invoice = shop.invoice(&t, &acme, "2026-01-10", "1000.00").await;
    let item = shop.item_of(&t, &acme, &invoice).await;
    let draft = shop
        .draft(
            &t,
            "dunning_run",
            "2026-02-15",
            json!({ "lines": [], "letters": [] }),
        )
        .await;
    let proposed = run(&t, "dunning_run.propose", json!({ "id": id(&draft) })).await;
    assert_eq!(
        proposed["body"]["lines"].as_array().unwrap().len(),
        1,
        "{proposed}"
    );
    // The customer pays everything, dated before the run.
    let (status, paid) = shop.pay(&t, "2026-02-14", &item, "1000.00").await;
    assert_eq!(status, StatusCode::OK, "{paid}");
    let (status, posted) = t
        .command(&t.admin, "dunning_run.post", json!({ "id": id(&draft) }))
        .await;
    let charged = if status == StatusCode::OK {
        let letter = posted["body"]["letters"][0]["letter_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (_, letter) = t
            .get(&t.admin, &format!("/api/documents/dunning_letter/{letter}"))
            .await;
        dec(&letter["body"]["fee"]) + dec(&letter["body"]["interest"])
    } else {
        0.0
    };
    assert!(
        status != StatusCode::OK || charged == 0.0,
        "a paid item was dunned: fee and interest {charged} charged; {posted}"
    );
    t.stop().await;
}

/// The letter's level (and so its fee and text) is the highest of all its
/// lines, skipped lines included.
#[tokio::test]
async fn a_skipped_line_does_not_raise_the_letter_level() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    shop.levels(&t).await;
    let acme = shop.customer(&t, "ACME").await;
    let old = shop.invoice(&t, &acme, "2026-01-10", "1000.00").await; // 36 days: level 2
    let old_item = shop.item_of(&t, &acme, &old).await;
    shop.invoice(&t, &acme, "2026-02-05", "100.00").await; // 10 days: level 1
    let draft = shop
        .draft(
            &t,
            "dunning_run",
            "2026-02-15",
            json!({ "lines": [], "letters": [] }),
        )
        .await;
    let proposed = run(&t, "dunning_run.propose", json!({ "id": id(&draft) })).await;
    assert_eq!(proposed["body"]["letters"][0]["level"], 2, "{proposed}");
    // Skip the level-2 item; the letter is now about a level-1 item only.
    let mut body = proposed["body"].clone();
    for line in body["lines"].as_array_mut().unwrap() {
        if line["open_item_id"] == json!(old_item) {
            line["skip"] = json!(true);
        }
    }
    body["letters"][0].as_object_mut().unwrap().remove("fee");
    let (status, changed) = t
        .patch(
            &t.admin,
            &format!("/api/documents/dunning_run/{}", id(&draft)),
            json!({ "version": proposed["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(
        changed["body"]["letters"][0]["level"], 1,
        "the letter keeps the skipped line's level and fee: {}",
        changed["body"]["letters"]
    );
    t.stop().await;
}

/// A payment dated before the invoice it settles: the statement does not
/// add up and cannot be issued (an internal error), for the customer and
/// for issue_all.
#[tokio::test]
async fn a_statement_with_a_payment_dated_before_its_invoice() {
    let t = TestApp::start().await;
    let shop = shop(&t).await;
    let acme = shop.customer(&t, "ACME").await;
    let invoice = shop.invoice(&t, &acme, "2026-04-02", "100.00").await;
    let item = shop.item_of(&t, &acme, &invoice).await;
    let (status, paid) = shop.pay(&t, "2026-03-30", &item, "100.00").await;
    if status != StatusCode::OK {
        // The payments module refuses it: nothing to check.
        eprintln!("payment dated before its invoice refused: {paid}");
        t.stop().await;
        return;
    }
    let (status, body) = t
        .command(
            &t.admin,
            "customer_statement.issue_all",
            json!({ "company_id": shop.company, "from_date": "2026-03-01", "date": "2026-03-31" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "March statements: {body}");
    let (status, body) = t
        .command(
            &t.admin,
            "customer_statement.issue_all",
            json!({ "company_id": shop.company, "from_date": "2026-04-01", "date": "2026-04-30" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "April statements: {body}");
    t.stop().await;
}
