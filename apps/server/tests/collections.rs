//! Collections: a customer statement that adds up, and dunning runs that
//! climb the levels, charge fees (taxed where the customer is) and
//! interest, respect holds and payment plans, ask before waiving, keep
//! each item's history and send the letters. The books are checked: they
//! balance, the receivables equal their account, and the tax report
//! equals the tax account.

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

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

struct Shop {
    company: String,
    revenue: String,
    bank: String,
    accounts: BTreeMap<String, String>,
}

/// A server that sends mail to an SMTP server on this host, which it does
/// only when it allows private addresses.
async fn start() -> TestApp {
    // SAFETY: set before the server starts; only the mail module reads it,
    // as a server starts, and every test here sets the same value.
    unsafe { std::env::set_var(erpcraft_mail::ALLOW_PRIVATE, "1") };
    TestApp::start().await
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
    async fn customer(&self, t: &TestApp, code: &str, email: Option<&str>) -> String {
        let id = t
            .create(
                "customer",
                json!({ "code": code, "name": format!("{code} Co") }),
            )
            .await;
        let mut input = json!({
            "party_type": "customer", "party_id": id,
            "addresses": [{ "kind": "billing", "is_default": true, "street": "1 High St",
                            "city": "Columbus", "postcode": "43215", "region": "OH",
                            "country_code": "US" }],
        });
        if let Some(email) = email {
            input["contacts"] = json!([{ "name": "Payables", "email": email, "is_default": true }]);
        }
        run(t, "party.set", input).await;
        id
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

    /// The customer's open receivable of a document.
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
            .unwrap_or_else(|| panic!("no item of {}: {list}", document["id"]))["id"]
            .as_str()
            .unwrap()
            .to_owned()
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

    /// The books balance and the receivables equal their account. The
    /// balances by account code.
    async fn books(&self, t: &TestApp) -> BTreeMap<String, f64> {
        let tb = run(t, "gl.trial_balance", json!({ "company_id": self.company })).await;
        assert_eq!(tb["total_debit"], tb["total_credit"], "the books balance");
        let balances: BTreeMap<String, f64> = tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["code"].as_str().unwrap().to_owned(), dec(&r["balance"])))
            .collect();
        let items = run(
            t,
            "open_item.list",
            json!({ "company_id": self.company, "kind": "receivable" }),
        )
        .await;
        assert!(
            close(dec(&items["total_open"]), balances["1200"]),
            "open receivables equal 1200: {items}"
        );
        balances
    }
}

#[tokio::test]
async fn a_statement_shows_the_period_and_adds_up() {
    let t = start().await;
    let shop = shop(&t).await;
    let customer = shop.customer(&t, "ACME", Some("ap@acme.example")).await;
    let first = shop.invoice(&t, &customer, "2026-01-10", "1000.00").await;
    let second = shop.invoice(&t, &customer, "2026-02-20", "500.00").await;
    let first_item = shop.item_of(&t, &customer, &first).await;
    shop.pay(&t, "2026-03-05", &first_item, "300.00").await;
    // After the period: not on the statement.
    shop.invoice(&t, &customer, "2026-04-02", "75.00").await;

    let draft = shop
        .draft(
            &t,
            "customer_statement",
            "2026-03-31",
            json!({ "customer_id": customer, "from_date": "2026-02-01" }),
        )
        .await;
    let body = &draft["body"];
    assert!(close(dec(&body["opening"]), 1000.0), "{body}");
    let activity: Vec<(String, f64, f64, f64)> = body["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["kind"].as_str().unwrap().to_owned(),
                dec(&a["debit"]),
                dec(&a["credit"]),
                dec(&a["balance"]),
            )
        })
        .collect();
    assert_eq!(
        activity,
        vec![
            ("item".into(), 500.0, 0.0, 1500.0),
            ("settlement".into(), 0.0, 300.0, 1200.0),
        ],
        "{body}"
    );
    assert_eq!(body["activity"][0]["number"], second["number"]);
    assert!(close(dec(&body["closing"]), 1200.0), "{body}");
    // Ageing on March 31: 700 of the first, 80 days overdue; 500 of the
    // second, 39 days.
    assert!(close(dec(&body["overdue_61_90"]), 700.0), "{body}");
    assert!(close(dec(&body["overdue_31_60"]), 500.0), "{body}");
    assert_eq!(body["open_items"].as_array().unwrap().len(), 2, "{body}");

    let issued = run(&t, "customer_statement.issue", json!({ "id": id(&draft) })).await;
    assert_eq!(issued["status"], "issued");
    assert_eq!(issued["locked"], true);
    // It prints, and it is sent to the customer.
    let page = run(
        &t,
        "document.render",
        json!({ "doc_type": "customer_statement", "id": id(&draft) }),
    )
    .await;
    assert!(
        page["html"].as_str().unwrap().contains("1,200.00")
            || page["html"].as_str().unwrap().contains("1200.00"),
        "{page}"
    );
    run(
        &t,
        "smtp_settings.set",
        json!({ "company_id": shop.company, "host": "127.0.0.1", "port": 2525,
                "security": "none", "from_address": "ar@ohio.example" }),
    )
    .await;
    let sent = run(
        &t,
        "document.send",
        json!({ "doc_type": "customer_statement", "id": id(&draft) }),
    )
    .await;
    assert!(sent["id"].is_string(), "{sent}");

    // A month's statements in one command: one customer with something.
    let all = run(
        &t,
        "customer_statement.issue_all",
        json!({ "company_id": shop.company, "from_date": "2026-04-01", "date": "2026-04-30" }),
    )
    .await;
    assert_eq!(all["statements"].as_array().unwrap().len(), 1, "{all}");
    t.stop().await;
}

#[tokio::test]
async fn dunning_runs_climb_the_levels_and_post_fees_and_interest() {
    let t = start().await;
    let shop = shop(&t).await;
    // Ohio sales tax on the fee of the last level, for the test.
    run(
        &t,
        "tax_area.import",
        json!({ "csv": "country,region,code,rate\nUS,OH,US-OH-T,5.75\n",
                "company_id": shop.company, "collected_account_id": shop.accounts["2100"],
                "paid_account_id": shop.accounts["6130"] }),
    )
    .await;
    t.create(
        "tax_registration",
        json!({ "company_id": shop.company, "country_code": "US", "region": "OH",
                "valid_from": "2026-03-01" }),
    )
    .await;
    let other_income = &shop.accounts["4900"];
    for (level, days, fee, rate, taxable) in [
        (1, 7, "0", "0", false),
        (2, 30, "25.00", "12", false),
        (3, 60, "50.00", "12", true),
    ] {
        t.create(
            "dunning_level",
            json!({ "company_id": shop.company, "level": level, "name": format!("Reminder {level}"),
                    "days_overdue": days, "fee": fee, "fee_account_id": other_income,
                    "fee_taxable": taxable, "interest_rate": rate,
                    "interest_account_id": other_income, "pay_within_days": 10,
                    "subject": format!("Reminder {level}"),
                    "text": "Please pay what is overdue." }),
        )
        .await;
    }
    // A lower level reached later than a higher one is refused.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/dunning_level",
            json!({ "company_id": shop.company, "level": 4, "name": "Late", "days_overdue": 45 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let acme = shop.customer(&t, "ACME", Some("ap@acme.example")).await;
    let held = shop.customer(&t, "HELD", None).await;
    let planned = shop.customer(&t, "PLAN", None).await;
    let invoice = shop.invoice(&t, &acme, "2026-01-10", "1000.00").await;
    let item = shop.item_of(&t, &acme, &invoice).await;
    shop.invoice(&t, &held, "2026-01-10", "200.00").await;
    shop.invoice(&t, &planned, "2026-01-10", "300.00").await;
    t.create(
        "dunning_hold",
        json!({ "company_id": shop.company, "customer_id": held, "reason": "Disputed" }),
    )
    .await;
    t.create(
        "payment_plan",
        json!({ "company_id": shop.company, "customer_id": planned, "starts_on": "2026-02-01",
                "note": "100 a month" }),
    )
    .await;

    // February 15: 36 days overdue, level 2 at once.
    let draft = shop
        .draft(
            &t,
            "dunning_run",
            "2026-02-15",
            json!({ "lines": [], "letters": [] }),
        )
        .await;
    let proposed = run(&t, "dunning_run.propose", json!({ "id": id(&draft) })).await;
    let lines = proposed["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1, "only ACME: {proposed}");
    assert_eq!(lines[0]["level"], 2);
    // 1000 at 12 % for the 36 days from January 11 to February 15.
    assert!(
        close(dec(&lines[0]["proposed_interest"]), 11.84),
        "{proposed}"
    );
    assert!(
        close(dec(&proposed["body"]["letters"][0]["fee"]), 25.0),
        "{proposed}"
    );

    // Waiving the fee asks the policy, which refuses by default.
    let mut body = proposed["body"].clone();
    body["letters"][0]["fee"] = json!("0");
    let (status, changed) = t
        .patch(
            &t.admin,
            &format!("/api/documents/dunning_run/{}", id(&draft)),
            json!({ "version": proposed["version"], "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let (status, refused) = t
        .command(&t.admin, "dunning_run.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(refused.to_string().contains("waive"), "{refused}");
    // Whoever the company allows may.
    t.create(
        "policy_rule",
        json!({ "company_id": shop.company, "point": "dunning.waive", "effect": "allow",
                "condition": "facts.fee_waived <= 25" }),
    )
    .await;
    let posted = run(&t, "dunning_run.post", json!({ "id": id(&draft) })).await;
    assert_eq!(posted["status"], "posted");
    let letter_id = posted["body"]["letters"][0]["letter_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, letter) = t
        .get(
            &t.admin,
            &format!("/api/documents/dunning_letter/{letter_id}"),
        )
        .await;
    assert_eq!(letter["status"], "posted", "{letter}");
    assert!(close(dec(&letter["body"]["interest"]), 11.84), "{letter}");
    assert!(close(dec(&letter["body"]["fee"]), 0.0), "{letter}");
    let balances = shop.books(&t).await;
    assert!(close(balances["4900"], -11.84), "{balances:?}");

    // A letter is not made by hand.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/documents/dunning_letter",
            json!({ "company_id": shop.company, "doc_date": "2026-02-15",
                    "body": { "customer_id": acme } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // March 25: 74 days, level 3, with its taxed fee and interest from
    // February 16; the interest item of February is at level 1.
    let draft = shop
        .draft(
            &t,
            "dunning_run",
            "2026-03-25",
            json!({ "lines": [], "letters": [] }),
        )
        .await;
    let proposed = run(&t, "dunning_run.propose", json!({ "id": id(&draft) })).await;
    let lines = proposed["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "{proposed}");
    let invoice_line = lines
        .iter()
        .find(|l| l["open_item_id"] == json!(item))
        .unwrap();
    assert_eq!(invoice_line["level"], 3);
    assert_eq!(invoice_line["interest_from"], "2026-02-16");
    // 1000 at 12 % for 38 days.
    assert!(close(dec(&invoice_line["interest"]), 12.49), "{proposed}");
    assert_eq!(proposed["body"]["letters"][0]["level"], 3);

    // A hold placed after the proposal stops the run.
    let hold = t
        .create(
            "dunning_hold",
            json!({ "company_id": shop.company, "open_item_id": item, "reason": "Promised" }),
        )
        .await;
    let (status, refused) = t
        .command(&t.admin, "dunning_run.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(refused.to_string().contains("on hold"), "{refused}");
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/dunning_hold/{hold}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let posted = run(&t, "dunning_run.post", json!({ "id": id(&draft) })).await;
    let letter_id = posted["body"]["letters"][0]["letter_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, letter) = t
        .get(
            &t.admin,
            &format!("/api/documents/dunning_letter/{letter_id}"),
        )
        .await;
    // Fee 50 with Ohio's 5.75 %, and interest.
    assert!(close(dec(&letter["body"]["fee"]), 50.0), "{letter}");
    assert!(close(dec(&letter["body"]["tax"]), 2.88), "{letter}");
    assert!(
        close(dec(&letter["summary"]["total"]), 50.0 + 2.88 + 12.49),
        "{letter}"
    );

    // The item's history: level 2, then 3.
    let (_, history) = t
        .get(
            &t.admin,
            &format!("/api/entities/dunning_history?open_item_id={item}"),
        )
        .await;
    let levels: Vec<i64> = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["level"].as_i64().unwrap())
        .collect();
    assert_eq!(levels, vec![2, 3], "{history}");
    // Nothing reaches a new level a day later.
    let draft = shop
        .draft(
            &t,
            "dunning_run",
            "2026-03-26",
            json!({ "lines": [], "letters": [] }),
        )
        .await;
    let proposed = run(&t, "dunning_run.propose", json!({ "id": id(&draft) })).await;
    assert_eq!(proposed["body"]["lines"], json!([]), "{proposed}");
    run(&t, "dunning_run.cancel", json!({ "id": id(&draft) })).await;

    // The books: fees and interest on 4900, the fee's tax on 2100, and the
    // tax report equals the account.
    let balances = shop.books(&t).await;
    assert!(
        close(balances["4900"], -(11.84 + 50.0 + 12.49)),
        "{balances:?}"
    );
    let report = run(
        &t,
        "tax.report",
        json!({ "company_id": shop.company, "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(
        close(dec(&report["total_collected"]), -balances["2100"]),
        "{report} {balances:?}"
    );
    assert!(close(dec(&report["total_collected"]), 2.88), "{report}");

    // The letters go to the customer by mail.
    run(
        &t,
        "smtp_settings.set",
        json!({ "company_id": shop.company, "host": "127.0.0.1", "port": 2525,
                "security": "none", "from_address": "ar@ohio.example" }),
    )
    .await;
    let sent = run(&t, "dunning_run.send", json!({ "id": id(&posted) })).await;
    assert_eq!(sent["sent"].as_array().unwrap().len(), 1, "{sent}");
    assert!(sent["sent"][0]["message_id"].is_string(), "{sent}");
    t.stop().await;
}
