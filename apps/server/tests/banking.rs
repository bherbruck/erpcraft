//! Bank statements: imported from the three formats, matched to what the
//! books hold, reconciled to the cent, and locked.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

const MARCH: &str = include_str!("../../../modules/banking/tests/data/march.camt053.xml");
const APRIL: &str = include_str!("../../../modules/banking/tests/data/april.ofx");
const MAY: &str = include_str!("../../../modules/banking/tests/data/may.csv");

async fn run(t: &TestApp, command: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

/// Draft a document and run `command` on it.
async fn posted(t: &TestApp, company: &str, doc_type: &str, command: &str, body: Value) -> Value {
    let (status, draft) = t
        .post(
            &t.admin,
            &format!("/api/documents/{doc_type}"),
            json!({ "company_id": company, "doc_date": "2026-03-15", "body": body }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{doc_type}: {draft}");
    run(t, command, json!({ "id": id(&draft) })).await
}

struct Books {
    company: String,
    bank: String,
    account: std::collections::HashMap<String, String>,
}

impl Books {
    fn account(&self, code: &str) -> String {
        self.account[code].clone()
    }
}

/// A US company with a bank account, a vendor invoice of 40.00 paid from
/// the bank on 15 March, and a sales invoice of 36.00 still open.
async fn books(t: &TestApp) -> Books {
    let (status, created) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "us_small_business", "code": "ACME", "name": "Acme",
                    "first_year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let company = created["company"]["id"].as_str().unwrap().to_owned();
    let bank = created["roles"]["bank"]["id"].as_str().unwrap().to_owned();
    let listed = run(
        t,
        "entity.list",
        json!({ "entity": "account", "filters": { "company_id": company }, "limit": 500 }),
    )
    .await;
    let account = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["code"].as_str().unwrap().to_owned(),
                a["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let warehouse = created["warehouse_id"].clone();
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let customer = t
        .create("customer", json!({ "code": "C001", "name": "Buyer Ltd" }))
        .await;

    let order = posted(
        t,
        &company,
        "purchase_order",
        "purchase_order.approve",
        json!({ "vendor_id": vendor, "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "10", "unit_price": "4.00" }] }),
    )
    .await;
    let line = order["body"]["lines"][0]["id"].clone();
    posted(
        t,
        &company,
        "purchase_receipt",
        "purchase_receipt.post",
        json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10" }] }),
    )
    .await;
    let bill = posted(
        t,
        &company,
        "vendor_invoice",
        "vendor_invoice.post",
        json!({ "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "10", "unit_price": "4.00" }] }),
    )
    .await;
    assert_eq!(bill["number"], "VI-2026-00001");
    let draft = run(
        t,
        "payment.draft",
        json!({ "company_id": company, "open_item_ids": open_items(t, &company, "payable").await,
                "bank_account_id": bank, "doc_date": "2026-03-15" }),
    )
    .await;
    run(t, "payment.post", json!({ "id": draft["id"] })).await;

    let order = posted(
        t,
        &company,
        "sales_order",
        "sales_order.approve",
        json!({ "customer_id": customer, "lines": [{ "item_id": item, "warehouse_id": warehouse, "qty": "4", "unit_price": "9.00" }] }),
    )
    .await;
    let line = order["body"]["lines"][0]["id"].clone();
    posted(
        t,
        &company,
        "sales_delivery",
        "sales_delivery.post",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "4" }] }),
    )
    .await;
    let invoice = posted(
        t,
        &company,
        "sales_invoice",
        "sales_invoice.post",
        json!({ "sales_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "4", "unit_price": "9.00" }] }),
    )
    .await;
    assert_eq!(invoice["number"], "SI-2026-00001");
    // The numbers the bank's files name the account by.
    t.create(
        "bank_account_number",
        json!({ "company_id": company, "bank_account_id": bank,
                "iban": "US00SAMPLE0000000001", "account_number": "SAMPLE0001",
                "bic": "SAMPUS33", "account_name": "Acme Operating" }),
    )
    .await;
    Books {
        company,
        bank,
        account,
    }
}

async fn open_items(t: &TestApp, company: &str, kind: &str) -> Vec<Value> {
    let list = run(
        t,
        "open_item.list",
        json!({ "company_id": company, "kind": kind }),
    )
    .await;
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].clone())
        .collect()
}

async fn import(t: &TestApp, bank: &str, format: &str, content: &str) -> (StatusCode, Value) {
    t.command(
        &t.admin,
        "bank_statement.import",
        json!({ "bank_account_id": bank, "format": format, "content": content }),
    )
    .await
}

async fn reconciliation(t: &TestApp, bank: &str, as_of: &str) -> Value {
    let report = run(
        t,
        "bank.reconciliation",
        json!({ "bank_account_id": bank, "as_of": as_of }),
    )
    .await;
    // The difference is always explained to the cent.
    assert_eq!(dec(&report["unexplained"]), 0.0, "{report}");
    assert_eq!(report["difference"], report["explained"], "{report}");
    report
}

fn line(statement: &Value, n: usize) -> Value {
    statement["body"]["lines"][n].clone()
}

async fn proposals(t: &TestApp, statement: &Value) -> Vec<Value> {
    let out = run(
        t,
        "bank_statement.proposals",
        json!({ "id": statement["id"] }),
    )
    .await;
    out["lines"].as_array().unwrap().clone()
}

#[tokio::test]
async fn a_statement_is_imported_matched_reconciled_and_locked() {
    let t = TestApp::start().await;
    let b = books(&t).await;

    // March, from camt.053.
    let (status, march) = import(&t, &b.bank, "camt053", MARCH).await;
    assert_eq!(status, StatusCode::OK, "{march}");
    assert_eq!(march["status"], "draft");
    assert_eq!(march["body"]["source_format"], "camt053");
    assert_eq!(march["body"]["opening_balance"], "1000.0000");
    assert_eq!(march["body"]["closing_balance"], "993.5000");
    assert_eq!(march["body"]["period_start"], "2026-03-01");
    let lines = march["body"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["amount"], "-40.0000");
    assert_eq!(lines[0]["counterparty_name"], "Parts Inc");
    assert_eq!(lines[0]["reference"], "Payment VI-2026-00001");
    assert_eq!(
        lines[1]["reference"],
        "Invoice SI-2026-00001 thanks & regards"
    );
    assert_eq!(lines[2]["reference"], "Account maintenance fee");
    assert_eq!(lines[2]["bank_transaction_id"], "CAMT-0003");

    // The same file again creates nothing.
    let (status, again) = import(&t, &b.bank, "camt053", MARCH).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["id"], march["id"]);
    assert_eq!(t.count("SELECT count(*) FROM bank_statement").await, 1);

    // Before matching: the payment of 15 March is outstanding, the three
    // lines unmatched, and the opening balance came before the books.
    let report = reconciliation(&t, &b.bank, "2026-03-31").await;
    assert_eq!(dec(&report["ledger_balance"]), -40.0);
    assert_eq!(dec(&report["statement_balance"]), 993.5);
    assert_eq!(dec(&report["first_opening_balance"]), 1000.0);
    assert_eq!(report["outstanding"].as_array().unwrap().len(), 1);
    assert_eq!(dec(&report["outstanding_total"]), -40.0);
    assert_eq!(dec(&report["unmatched_total"]), -6.5);
    assert_eq!(dec(&report["difference"]), -1033.5);

    // What the lines probably are.
    let proposed = proposals(&t, &march).await;
    let best = |n: usize| proposed[n]["proposals"][0].clone();
    assert_eq!(best(0)["kind"], "posting", "{proposed:?}");
    assert_eq!(best(0)["posting"]["doc_type"], "payment");
    assert_eq!(best(1)["kind"], "open_items", "{proposed:?}");
    assert_eq!(best(1)["numbers"], json!(["SI-2026-00001"]));
    assert_eq!(best(1)["party_name"], "Buyer Ltd");
    assert_eq!(best(1)["score"], 90);
    assert_eq!(best(2)["kind"], "account", "{proposed:?}");
    assert_eq!(best(2)["account_id"], Value::Null);

    // Accept each kind.
    let matched = run(
        &t,
        "bank_statement.match",
        json!({ "id": march["id"], "line_id": line(&march, 0)["id"], "action": "posting",
                "posting_id": best(0)["posting"]["posting_id"] }),
    )
    .await;
    assert_eq!(
        line(&matched, 0)["posting_id"],
        best(0)["posting"]["posting_id"]
    );
    let matched = run(
        &t,
        "bank_statement.match",
        json!({ "id": march["id"], "line_id": line(&march, 1)["id"], "action": "open_items",
                "open_item_ids": best(1)["open_item_ids"] }),
    )
    .await;
    let payment = line(&matched, 1)["payment_id"].clone();
    assert!(payment.is_string(), "{matched}");
    let fees = b.account("6020");
    let matched = run(
        &t,
        "bank_statement.match",
        json!({ "id": march["id"], "line_id": line(&march, 2)["id"], "action": "account",
                "account_id": fees }),
    )
    .await;
    assert_eq!(matched["summary"]["unmatched"], 0);
    // A posting is matched once.
    let (status, body) = t
        .command(
            &t.admin,
            "bank_statement.match",
            json!({ "id": march["id"], "line_id": line(&march, 2)["id"], "action": "posting",
                    "posting_id": best(0)["posting"]["posting_id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // The drafted payment must be posted before the statement.
    let (status, body) = t
        .command(
            &t.admin,
            "bank_statement.post",
            json!({ "id": march["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["line"], 2);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("post it first")
    );
    let paid = run(&t, "payment.post", json!({ "id": payment })).await;
    assert_eq!(dec(&paid["body"]["amount"]), 36.0);
    let reconciled = run(&t, "bank_statement.post", json!({ "id": march["id"] })).await;
    assert_eq!(reconciled["status"], "posted");
    assert_eq!(reconciled["number"], "BS-2026-00001");
    for n in 0..3 {
        assert!(
            line(&reconciled, n)["posting_id"].is_string(),
            "{reconciled}"
        );
    }
    // Locked.
    let (status, _) = t
        .command(
            &t.admin,
            "bank_statement.match",
            json!({ "id": march["id"], "line_id": line(&march, 2)["id"], "action": "clear" }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);

    // Reconciled: nothing outstanding, nothing unmatched, and the ledger
    // differs from the bank by the money that was there before.
    let report = reconciliation(&t, &b.bank, "2026-03-31").await;
    assert_eq!(dec(&report["ledger_balance"]), -6.5);
    assert_eq!(report["outstanding"], json!([]));
    assert_eq!(report["unmatched_lines"], json!([]));
    assert_eq!(dec(&report["difference"]), -1000.0);

    // April, from OFX: it opens where March closed; the fee is proposed to
    // the account March's fee went to.
    let (status, april) = import(&t, &b.bank, "ofx", APRIL).await;
    assert_eq!(status, StatusCode::OK, "{april}");
    assert_eq!(april["body"]["opening_balance"], "993.5000");
    assert_eq!(april["body"]["closing_balance"], "990.0000");
    assert_eq!(line(&april, 0)["bank_transaction_id"], "OFX-0001");
    let proposed = proposals(&t, &april).await;
    assert_eq!(proposed[0]["proposals"][0]["kind"], "account");
    assert_eq!(proposed[0]["proposals"][0]["account_id"], fees.as_str());
    run(
        &t,
        "bank_statement.match",
        json!({ "id": april["id"], "line_id": line(&april, 0)["id"], "action": "account",
                "account_id": fees }),
    )
    .await;
    run(&t, "bank_statement.post", json!({ "id": april["id"] })).await;

    // May, from CSV with the bank account's mapping. An office supplies
    // order of 19.50 was paid from the bank by journal entry; the bank
    // took 20.00.
    let (status, body) = import(&t, &b.bank, "csv", MAY).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("bank_csv_mapping")
    );
    t.create(
        "bank_csv_mapping",
        json!({
            "company_id": b.company, "bank_account_id": b.bank, "delimiter": ";",
            "has_header": true, "date_format": "%d.%m.%Y", "decimal_comma": true,
            "booking_date_column": "Buchungstag", "value_date_column": "Valuta",
            "amount_column": "Betrag", "counterparty_name_column": "Empfänger",
            "reference_column": "Verwendungszweck", "transaction_id_column": "Transaktion",
        }),
    )
    .await;
    let (status, may) = import(&t, &b.bank, "csv", MAY).await;
    assert_eq!(status, StatusCode::OK, "{may}");
    assert_eq!(may["body"]["opening_balance"], "990.0000");
    assert_eq!(may["body"]["closing_balance"], "971.2500");
    let (status, entry) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({ "company_id": b.company, "doc_date": "2026-05-09", "body": { "lines": [
                { "account_id": b.account("6050"), "debit": "19.50" },
                { "account_id": b.bank, "credit": "19.50" },
            ] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{entry}");
    let entry = run(&t, "journal_entry.post", json!({ "id": entry["id"] })).await;
    let posting = entry["postings"][0]["id"].clone();
    // Not the same amount: refused, unless the company accepts the
    // difference as a fee.
    let accept = json!({ "id": may["id"], "line_id": line(&may, 0)["id"], "action": "posting",
                         "posting_id": posting, "account_id": fees });
    let (status, body) = t
        .command(&t.admin, "bank_statement.match", accept.clone())
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    t.create(
        "policy_rule",
        json!({ "company_id": b.company, "point": "bank_statement.amount_difference",
                "effect": "allow", "condition": "facts.difference_abs <= 1" }),
    )
    .await;
    run(&t, "bank_statement.match", accept).await;
    run(
        &t,
        "bank_statement.match",
        json!({ "id": may["id"], "line_id": line(&may, 1)["id"], "action": "account",
                "account_id": b.account("4900") }),
    )
    .await;
    // Before posting, the 0.50 the bank kept is unmatched.
    let report = reconciliation(&t, &b.bank, "2026-05-31").await;
    assert_eq!(dec(&report["unmatched_total"]), 0.75);
    let may = run(&t, "bank_statement.post", json!({ "id": may["id"] })).await;
    assert!(line(&may, 0)["difference_posting_id"].is_string(), "{may}");
    let report = reconciliation(&t, &b.bank, "2026-05-31").await;
    assert_eq!(report["unmatched_lines"], json!([]));
    assert_eq!(report["outstanding"], json!([]));
    assert_eq!(dec(&report["statement_balance"]), 971.25);
    assert_eq!(dec(&report["ledger_balance"]), -28.75);

    // The books.
    let tb = run(&t, "gl.trial_balance", json!({ "company_id": b.company })).await;
    assert_eq!(tb["total_debit"], tb["total_credit"]);
    let balance = |code: &str| {
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["code"] == code)
            .map_or(0.0, |r| dec(&r["balance"]))
    };
    assert_eq!(balance("1020"), -28.75);
    assert_eq!(balance("6020"), 2.5 + 3.5 + 0.5);
    assert_eq!(balance("4900"), -1.25);
    assert_eq!(balance("1200"), 0.0, "the customer's invoice is paid");
    let receivables = run(
        &t,
        "open_item.list",
        json!({ "company_id": b.company, "kind": "receivable" }),
    )
    .await;
    assert_eq!(dec(&receivables["total_open"]), balance("1200"));
    let payables = run(
        &t,
        "open_item.list",
        json!({ "company_id": b.company, "kind": "payable" }),
    )
    .await;
    assert_eq!(dec(&payables["total_open"]), -balance("2010"));
    t.stop().await;
}

#[tokio::test]
async fn a_statement_must_add_up_and_follow_the_one_before() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let statement = |opening: &str, closing: &str, start: &str, end: &str| {
        json!({ "company_id": b.company, "doc_date": end, "body": {
            "bank_account_id": b.bank, "period_start": start, "period_end": end,
            "opening_balance": opening, "closing_balance": closing,
            "lines": [{ "booking_date": start, "amount": "-10.00", "bank_transaction_id": format!("T-{start}") }],
        } })
    };
    // Opening plus lines is not the closing balance.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            statement("100.00", "100.00", "2026-01-01", "2026-01-31"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "closing_balance");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            statement("100.00", "90.00", "2026-01-01", "2026-01-31"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // The next must open where it closed, and start after it.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            statement("95.00", "85.00", "2026-02-01", "2026-02-28"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "opening_balance");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            statement("90.00", "80.00", "2026-01-15", "2026-02-15"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "period_start");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            statement("90.00", "80.00", "2026-02-01", "2026-02-28"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // Not an account that holds money.
    let mut wrong = statement("80.00", "70.00", "2026-03-01", "2026-03-31");
    wrong["body"]["bank_account_id"] = json!(b.account("6020"));
    let (status, body) = t
        .post(&t.admin, "/api/documents/bank_statement", wrong)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "bank_account_id");
    // A file that repeats part of what was imported is refused.
    let (status, march) = import(&t, &b.bank, "camt053", MARCH).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{march}");
    t.stop().await;
}

#[tokio::test]
async fn malformed_files_are_refused_with_where() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let broken = MARCH.replace(
        "<Ntry>\n        <Amt Ccy=\"USD\">36.00</Amt>",
        "<Ntry>\n        <Amt Ccy=\"USD\">36.00</Amtx>",
    );
    let (status, body) = import(&t, &b.bank, "camt053", &broken).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("line 46"), "{message}");
    // An entity that would read a file is refused.
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        "<!DOCTYPE d [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><Document>&x;</Document>",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Another currency than the account's.
    let (status, body) = import(&t, &b.bank, "camt053", &MARCH.replace("USD", "EUR")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("in EUR")
    );
    // OFX without a statement, and a transaction without an amount.
    let (status, body) = import(
        &t,
        &b.bank,
        "ofx",
        "<OFX><SIGNONMSGSRSV1></SIGNONMSGSRSV1></OFX>",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = import(&t, &b.bank, "ofx", &APRIL.replace("<TRNAMT>-3.50", "")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("TRNAMT"),
        "{body}"
    );
    // A CSV date that is not one.
    t.create(
        "bank_csv_mapping",
        json!({
            "company_id": b.company, "bank_account_id": b.bank, "delimiter": ";",
            "has_header": true, "date_format": "%d.%m.%Y", "decimal_comma": true,
            "booking_date_column": "Buchungstag", "amount_column": "Betrag",
            "transaction_id_column": "Transaktion",
        }),
    )
    .await;
    let (status, body) = import(
        &t,
        &b.bank,
        "csv",
        &MAY.replace("12.05.2026;12", "32.05.2026;12"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("line 3"),
        "{body}"
    );
    assert_eq!(t.count("SELECT count(*) FROM bank_statement").await, 0);

    // A statement in a foreign currency, on a bank account in it.
    let eur = t
        .create(
            "account",
            json!({ "company_id": b.company, "code": "1040", "name": "Euro account",
                    "account_type": "asset", "currency": "EUR", "is_bank": true }),
        )
        .await;
    let (status, body) = import(&t, &eur, "camt053", MARCH).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let euro_file = MARCH
        .replace("USD", "EUR")
        .replace("US00SAMPLE0000000001", "DE00SAMPLE0000000002");
    // Not before the account's number is recorded.
    let (status, body) = import(&t, &eur, "camt053", &euro_file).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("record its IBAN or account number as a bank_account_number"),
        "{body}"
    );
    t.create(
        "bank_account_number",
        json!({ "company_id": b.company, "bank_account_id": eur,
                "iban": "DE00SAMPLE0000000002" }),
    )
    .await;
    // The euro account's statement is not the dollar account's.
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replace("US00SAMPLE0000000001", "DE00SAMPLE0000000002"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not this bank account's IBAN or account number"),
        "{body}"
    );
    let (status, body) = import(&t, &eur, "camt053", &euro_file).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["body"]["currency"], "EUR");
    t.stop().await;
}

#[tokio::test]
async fn banking_takes_its_permissions() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let (status, statement) = import(&t, &b.bank, "camt053", MARCH).await;
    assert_eq!(status, StatusCode::OK);
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!(["account:read"]),
            Some(&b.company),
        )
        .await;
    for (command, input) in [
        (
            "bank_statement.import",
            json!({ "bank_account_id": b.bank, "format": "ofx", "content": APRIL }),
        ),
        ("bank_statement.proposals", json!({ "id": statement["id"] })),
        (
            "bank_statement.match",
            json!({ "id": statement["id"], "line_id": line(&statement, 0)["id"], "action": "clear" }),
        ),
        ("bank_statement.post", json!({ "id": statement["id"] })),
        (
            "bank.reconciliation",
            json!({ "bank_account_id": b.bank, "as_of": "2026-03-31" }),
        ),
    ] {
        let (status, body) = t.command(&clerk, command, input).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{command}: {body}");
    }
    // Someone who may match lines but not pay may not draft a payment.
    let (matcher, _) = t
        .user_with_rules(
            "matcher@example.com",
            json!([
                "bank_statement:read",
                "bank_statement:match",
                "open_item:read"
            ]),
            Some(&b.company),
        )
        .await;
    let items = open_items(&t, &b.company, "receivable").await;
    let (status, body) = t
        .command(
            &matcher,
            "bank_statement.match",
            json!({ "id": statement["id"], "line_id": line(&statement, 1)["id"],
                    "action": "open_items", "open_item_ids": items }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn a_payment_drafted_for_one_line_explains_no_other() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let (status, march) = import(&t, &b.bank, "camt053", MARCH).await;
    assert_eq!(status, StatusCode::OK, "{march}");
    let proposed = proposals(&t, &march).await;
    let items = proposed[1]["proposals"][0]["open_item_ids"].clone();

    // Line 2 pays the customer's invoice: a payment is drafted and posted.
    let matched = run(
        &t,
        "bank_statement.match",
        json!({ "id": march["id"], "line_id": line(&march, 1)["id"], "action": "open_items",
                "open_item_ids": items }),
    )
    .await;
    let payment = line(&matched, 1)["payment_id"].as_str().unwrap().to_owned();
    run(&t, "payment.post", json!({ "id": payment })).await;
    let posting: String = sqlx::query_scalar(&format!(
        "SELECT id::text FROM posting WHERE document_id = '{payment}'"
    ))
    .fetch_one(&t.pool)
    .await
    .unwrap();

    // Its posting cannot explain another line too, nor be proposed for one.
    let (status, body) = t
        .command(
            &t.admin,
            "bank_statement.match",
            json!({ "id": march["id"], "line_id": line(&march, 2)["id"], "action": "posting",
                    "posting_id": posting, "account_id": b.account("6020") }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is matched to a line of statement"),
        "{body}"
    );
    for line in proposals(&t, &march).await {
        for proposal in line["proposals"].as_array().unwrap() {
            assert_ne!(
                proposal["posting"]["posting_id"],
                json!(posting),
                "{proposal}"
            );
        }
    }

    // And the database holds to it: a posting reconciles one line.
    let doubled = sqlx::query(&format!(
        "UPDATE bank_statement_line SET posting_id = '{posting}'
         WHERE document_id = '{}'",
        march["id"].as_str().unwrap()
    ))
    .execute(&t.pool)
    .await;
    let error = doubled.unwrap_err().to_string();
    assert!(error.contains("bank_statement_line_posting"), "{error}");
    t.stop().await;
}

#[tokio::test]
async fn hostile_statement_files_are_refused_not_believed() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let refused = |status: StatusCode, body: &Value, says: &str| {
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(says), "{message}");
        assert!(message.len() < 400, "{message}");
    };

    // Amounts beyond what a statement holds are refused, not summed into
    // a panic: in a file, and in a statement written directly.
    let huge = MARCH.replace(">36.00<", ">79000000000000000000000000000<");
    let (status, body) = import(&t, &b.bank, "camt053", &huge).await;
    refused(status, &body, "more than an amount can be");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/documents/bank_statement",
            json!({ "company_id": b.company, "doc_date": "2026-01-31", "body": {
                "bank_account_id": b.bank, "period_start": "2026-01-01",
                "period_end": "2026-01-31", "opening_balance": "0",
                "closing_balance": "0",
                "lines": [
                    { "booking_date": "2026-01-02", "amount": "70000000000000000000000000000" },
                    { "booking_date": "2026-01-03", "amount": "70000000000000000000000000000" },
                ] } }),
        )
        .await;
    refused(status, &body, "more than an amount can be");

    // camt.053 writes a point, no thousands separator and no sign.
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replace(">36.00<", ">1,036.00<"),
    )
    .await;
    refused(status, &body, "not an amount as camt.053 writes one");
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replace(">36.00<", ">-36.00<"),
    )
    .await;
    refused(status, &body, "not an amount as camt.053 writes one");
    // Neither credit nor debit.
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replacen(
            "<CdtDbtInd>CRDT</CdtDbtInd>",
            "<CdtDbtInd>CRDTX</CdtDbtInd>",
            1,
        ),
    )
    .await;
    refused(status, &body, "not CRDT or DBIT");
    // An entry in another currency, wherever it stands.
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replace(
            "<Amt Ccy=\"USD\">36.00</Amt>",
            "<Amt Ccy=\"EUR\">36.00</Amt>",
        ),
    )
    .await;
    refused(status, &body, "the amount is in EUR, the statement in USD");

    // OFX: a decimal comma is read as one, not as a thousands separator.
    let comma = APRIL.replace("<TRNAMT>-3.50", "<TRNAMT>-3,50");
    let (status, body) = import(&t, &b.bank, "ofx", &comma).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(dec(&body["body"]["lines"][0]["amount"]), -3.5);
    assert_eq!(dec(&body["body"]["opening_balance"]), 993.5);
    let (status, _) = t
        .send(
            "DELETE",
            &format!(
                "/api/documents/bank_statement/{}",
                body["id"].as_str().unwrap()
            ),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert!(status.is_success(), "{status}");
    let (status, body) = import(
        &t,
        &b.bank,
        "ofx",
        &APRIL.replace("<TRNAMT>-3.50", "<TRNAMT>-1,003.50"),
    )
    .await;
    refused(status, &body, "not an amount as OFX writes one");
    // Cut off: without its balance, or with a transaction left open.
    let cut = &APRIL[..APRIL.find("<LEDGERBAL>").unwrap()];
    let (status, body) = import(&t, &b.bank, "ofx", cut).await;
    refused(status, &body, "cut off");
    let (status, body) = import(
        &t,
        &b.bank,
        "ofx",
        &APRIL.replace("<LEDGERBAL><BALAMT>990.00<DTASOF>20260430</LEDGERBAL>", ""),
    )
    .await;
    refused(status, &body, "no closing balance");
    let open = APRIL.replace(
        "Account maintenance fee</STMTTRN>",
        "Account maintenance fee<STMTTRN><TRNTYPE>FEE<DTPOSTED>20260430<TRNAMT>-1.00<FITID>OFX-0002</STMTTRN>",
    );
    let (status, body) = import(&t, &b.bank, "ofx", &open).await;
    refused(status, &body, "before the one before it is closed");
    // A file for another account, and one in no currency.
    let (status, body) = import(
        &t,
        &b.bank,
        "ofx",
        &APRIL.replace("SAMPLE0001", "OTHER0002"),
    )
    .await;
    refused(
        status,
        &body,
        "not this bank account's IBAN or account number",
    );
    let (status, body) = import(&t, &b.bank, "ofx", &APRIL.replace("<CURDEF>USD", "")).await;
    refused(status, &body, "names no currency");

    // A line booked outside the statement's period.
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replacen(
            "<BookgDt><Dt>2026-03-20</Dt>",
            "<BookgDt><Dt>2026-04-20</Dt>",
            1,
        ),
    )
    .await;
    refused(status, &body, "outside the statement's period");

    // A long cell is not repeated whole.
    let long = "9".repeat(5000);
    let (status, body) = import(
        &t,
        &b.bank,
        "camt053",
        &MARCH.replace(">36.00<", &format!(">{long}x<")),
    )
    .await;
    refused(status, &body, "…");

    // A file that names no account says so on the statement.
    let anonymous = APRIL
        .replace("<ACCTID>SAMPLE0001", "")
        .replace("OFX-0001", "OFX-0099");
    // A BIC is a BIC.
    let savings = t
        .create(
            "account",
            json!({ "company_id": b.company, "code": "1050", "name": "Savings",
                    "account_type": "asset", "is_bank": true }),
        )
        .await;
    let numbers = |bic: &str| {
        json!({ "company_id": b.company, "bank_account_id": savings,
                "iban": "US00SAMPLE0000000009", "bic": bic })
    };
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/bank_account_number",
            numbers("not a bic"),
        )
        .await;
    assert!(status.is_client_error(), "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/bank_account_number",
            numbers("SAMPUS33XXX"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = import(&t, &b.bank, "ofx", &anonymous).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["memo"].as_str().unwrap().contains("names no account"),
        "{body}"
    );
    t.stop().await;
}
