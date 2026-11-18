//! Financial reports: balance sheet, income statement and account ledger.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

/// A company with a chart large enough for a balance sheet.
struct Books {
    company: Company,
    loan: String,
    capital: String,
    rent: String,
    /// Document id of the sale.
    sale: String,
}

async fn account(t: &TestApp, company: &str, code: &str, name: &str, kind: &str) -> String {
    t.create(
        "account",
        json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
    )
    .await
}

/// Post a two-line journal entry and return the document id.
async fn journal(
    t: &TestApp,
    company: &str,
    date: &str,
    memo: &str,
    debit: &str,
    credit: &str,
    amount: &str,
) -> String {
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({
                "company_id": company,
                "doc_date": date,
                "memo": memo,
                "body": { "lines": [
                    { "account_id": debit, "debit": amount, "memo": memo },
                    { "account_id": credit, "credit": amount },
                ]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let doc = id(&draft);
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    doc
}

/// Owner invests 10000 on Jan 10, a sale of 2500 on Feb 5, rent of 800 on
/// Feb 20, a loan of 5000 on Mar 1. All through cash.
async fn books(t: &TestApp) -> Books {
    let company = t.company("ACME", "USD").await;
    let c = company.id.clone();
    let loan = account(t, &c, "2000", "Bank loan", "liability").await;
    let capital = account(t, &c, "3000", "Owner capital", "equity").await;
    let rent = account(t, &c, "5000", "Rent", "expense").await;
    // An account that is never used stays out of every report.
    account(t, &c, "5100", "Travel", "expense").await;

    let cash = company.cash.clone();
    journal(
        t,
        &c,
        "2026-01-10",
        "Owner invests",
        &cash,
        &capital,
        "10000.00",
    )
    .await;
    let sale = journal(
        t,
        &c,
        "2026-02-05",
        "Sale",
        &cash,
        &company.revenue,
        "2500.00",
    )
    .await;
    journal(t, &c, "2026-02-20", "Rent", &rent, &cash, "800.00").await;
    journal(t, &c, "2026-03-01", "Loan", &cash, &loan, "5000.00").await;
    Books {
        company,
        loan,
        capital,
        rent,
        sale,
    }
}

async fn query(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The rows of a section as `(account id, balance)`.
fn rows(section: &Value) -> Vec<(String, f64)> {
    section["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["account_id"].as_str().unwrap().to_owned(),
                dec(&row["balance"]),
            )
        })
        .collect()
}

/// The balance sheet's own totals agree with its sections.
fn assert_balanced(sheet: &Value) {
    let sum = |section: &Value| rows(section).iter().map(|(_, b)| b).sum::<f64>();
    for name in ["assets", "liabilities", "equity"] {
        assert_eq!(sum(&sheet[name]), dec(&sheet[name]["total"]), "{sheet}");
    }
    assert_eq!(
        dec(&sheet["total_assets"]),
        dec(&sheet["assets"]["total"]),
        "{sheet}"
    );
    assert_eq!(
        dec(&sheet["total_liabilities_and_equity"]),
        dec(&sheet["liabilities"]["total"])
            + dec(&sheet["equity"]["total"])
            + dec(&sheet["net_income"]),
        "{sheet}"
    );
    assert_eq!(
        dec(&sheet["total_assets"]),
        dec(&sheet["total_liabilities_and_equity"]),
        "{sheet}"
    );
}

#[tokio::test]
async fn the_balance_sheet_balances_on_every_date() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;

    // End of January: only the owner's investment.
    let sheet = query(
        &t,
        "gl.balance_sheet",
        json!({ "company_id": c.id, "as_of": "2026-01-31" }),
    )
    .await;
    assert_eq!(sheet["currency"], "USD");
    assert_eq!(sheet["as_of"], "2026-01-31");
    assert_eq!(rows(&sheet["assets"]), [(c.cash.clone(), 10000.0)]);
    assert!(rows(&sheet["liabilities"]).is_empty());
    assert_eq!(rows(&sheet["equity"]), [(b.capital.clone(), 10000.0)]);
    assert_eq!(dec(&sheet["net_income"]), 0.0);
    assert_eq!(dec(&sheet["total_assets"]), 10000.0);
    assert_balanced(&sheet);

    // End of February: the sale and the rent are in the result, which is
    // not an equity account yet.
    let sheet = query(
        &t,
        "gl.balance_sheet",
        json!({ "company_id": c.id, "as_of": "2026-02-28" }),
    )
    .await;
    assert_eq!(rows(&sheet["assets"]), [(c.cash.clone(), 11700.0)]);
    assert!(rows(&sheet["liabilities"]).is_empty());
    assert_eq!(dec(&sheet["equity"]["total"]), 10000.0);
    assert_eq!(dec(&sheet["net_income"]), 1700.0);
    assert_eq!(dec(&sheet["total_liabilities_and_equity"]), 11700.0);
    assert_balanced(&sheet);

    // The posting date itself is included.
    let sheet = query(
        &t,
        "gl.balance_sheet",
        json!({ "company_id": c.id, "as_of": "2026-03-01" }),
    )
    .await;
    assert_eq!(rows(&sheet["liabilities"]), [(b.loan.clone(), 5000.0)]);
    assert_balanced(&sheet);

    // No date: everything. Rows carry code and name; group accounts and
    // accounts without a balance are left out.
    let sheet = query(&t, "gl.balance_sheet", json!({ "company_id": c.id })).await;
    assert_eq!(sheet["as_of"], Value::Null);
    assert_eq!(rows(&sheet["assets"]), [(c.cash.clone(), 16700.0)]);
    assert_eq!(sheet["assets"]["rows"][0]["code"], "1000");
    assert_eq!(sheet["assets"]["rows"][0]["name"], "Cash");
    assert_eq!(rows(&sheet["liabilities"]), [(b.loan.clone(), 5000.0)]);
    assert_eq!(rows(&sheet["equity"]), [(b.capital.clone(), 10000.0)]);
    assert_eq!(dec(&sheet["net_income"]), 1700.0);
    assert_eq!(dec(&sheet["total_assets"]), 16700.0);
    assert_balanced(&sheet);

    // Before anything was posted: empty, and still balanced.
    let sheet = query(
        &t,
        "gl.balance_sheet",
        json!({ "company_id": c.id, "as_of": "2026-01-09" }),
    )
    .await;
    assert!(rows(&sheet["assets"]).is_empty());
    assert_eq!(dec(&sheet["total_assets"]), 0.0);
    assert_balanced(&sheet);

    t.stop().await;
}

#[tokio::test]
async fn the_income_statement_covers_only_its_range() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let statement = |from: &'static str, to: &'static str| {
        query(
            &t,
            "gl.income_statement",
            json!({ "company_id": c.id, "from": from, "to": to }),
        )
    };

    let february = statement("2026-02-01", "2026-02-28").await;
    assert_eq!(february["currency"], "USD");
    assert_eq!(rows(&february["income"]), [(c.revenue.clone(), 2500.0)]);
    assert_eq!(february["income"]["rows"][0]["code"], "4000");
    assert_eq!(dec(&february["income"]["total"]), 2500.0);
    assert_eq!(rows(&february["expenses"]), [(b.rent.clone(), 800.0)]);
    assert_eq!(dec(&february["expenses"]["total"]), 800.0);
    assert_eq!(dec(&february["net_income"]), 1700.0);

    // Both ends of the range are included; a day later is not.
    let sale_only = statement("2026-02-05", "2026-02-19").await;
    assert_eq!(rows(&sale_only["income"]), [(c.revenue.clone(), 2500.0)]);
    assert!(rows(&sale_only["expenses"]).is_empty());
    assert_eq!(dec(&sale_only["net_income"]), 2500.0);
    let rent_only = statement("2026-02-06", "2026-02-20").await;
    assert!(rows(&rent_only["income"]).is_empty());
    assert_eq!(dec(&rent_only["net_income"]), -800.0);

    // Nothing in January or March touches income or expenses.
    for (from, to) in [("2026-01-01", "2026-01-31"), ("2026-03-01", "2026-03-31")] {
        let empty = statement(from, to).await;
        assert!(rows(&empty["income"]).is_empty());
        assert!(rows(&empty["expenses"]).is_empty());
        assert_eq!(dec(&empty["income"]["total"]), 0.0);
        assert_eq!(dec(&empty["net_income"]), 0.0);
    }

    // A single day is a valid range; the other way round is not.
    let day = statement("2026-02-05", "2026-02-05").await;
    assert_eq!(dec(&day["net_income"]), 2500.0);
    let (status, body) = t
        .command(
            &t.admin,
            "gl.income_statement",
            json!({ "company_id": c.id, "from": "2026-03-01", "to": "2026-02-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // Both dates are required.
    let (status, body) = t
        .command(
            &t.admin,
            "gl.income_statement",
            json!({ "company_id": c.id, "from": "2026-03-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_reversal_shows_in_every_report() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;

    // Cancel the February sale in March.
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.reverse",
            json!({ "id": b.sale, "posting_date": "2026-03-10" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // February is as it was; March carries the cancellation.
    let february = query(
        &t,
        "gl.income_statement",
        json!({ "company_id": c.id, "from": "2026-02-01", "to": "2026-02-28" }),
    )
    .await;
    assert_eq!(dec(&february["net_income"]), 1700.0);
    let march = query(
        &t,
        "gl.income_statement",
        json!({ "company_id": c.id, "from": "2026-03-01", "to": "2026-03-31" }),
    )
    .await;
    assert_eq!(rows(&march["income"]), [(c.revenue.clone(), -2500.0)]);
    assert_eq!(dec(&march["net_income"]), -2500.0);
    // Over the year the sale and its reversal cancel out, so the revenue
    // account drops out and only the rent remains.
    let year = query(
        &t,
        "gl.income_statement",
        json!({ "company_id": c.id, "from": "2026-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert!(rows(&year["income"]).is_empty());
    assert_eq!(rows(&year["expenses"]), [(b.rent.clone(), 800.0)]);
    assert_eq!(dec(&year["net_income"]), -800.0);

    let before = query(
        &t,
        "gl.balance_sheet",
        json!({ "company_id": c.id, "as_of": "2026-03-09" }),
    )
    .await;
    assert_eq!(dec(&before["total_assets"]), 16700.0);
    assert_eq!(dec(&before["net_income"]), 1700.0);
    assert_balanced(&before);
    let after = query(&t, "gl.balance_sheet", json!({ "company_id": c.id })).await;
    assert_eq!(rows(&after["assets"]), [(c.cash.clone(), 14200.0)]);
    assert_eq!(dec(&after["net_income"]), -800.0);
    assert_eq!(dec(&after["total_liabilities_and_equity"]), 14200.0);
    assert_balanced(&after);

    // The cash ledger ends with the reversal, linked to the same document.
    let ledger = query(
        &t,
        "gl.account_ledger",
        json!({ "company_id": c.id, "account_id": c.cash }),
    )
    .await;
    assert_eq!(ledger["total"], 5);
    let last = &ledger["entries"][4];
    assert_eq!(last["posting_date"], "2026-03-10");
    assert_eq!(dec(&last["credit"]), 2500.0);
    assert_eq!(dec(&last["balance"]), 14200.0);
    assert_eq!(last["document_id"], b.sale.as_str());
    assert_eq!(last["posting_memo"], "Reversal of JE-2026-00002");
    assert_eq!(dec(&ledger["closing_balance"]), 14200.0);

    t.stop().await;
}

#[tokio::test]
async fn the_account_ledger_keeps_a_running_balance_across_pages() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let ledger = |extra: Value| {
        let mut input = json!({ "company_id": c.id, "account_id": c.cash });
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        query(&t, "gl.account_ledger", input)
    };
    let balances = |ledger: &Value| -> Vec<f64> {
        ledger["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| dec(&e["balance"]))
            .collect()
    };

    // The whole account: no opening balance.
    let all = ledger(json!({})).await;
    assert_eq!(all["code"], "1000");
    assert_eq!(all["name"], "Cash");
    assert_eq!(all["currency"], "USD");
    assert_eq!(dec(&all["opening_balance"]), 0.0);
    assert_eq!(all["total"], 4);
    assert_eq!(all["limit"], 50);
    assert_eq!(balances(&all), [10000.0, 12500.0, 11700.0, 16700.0]);
    assert_eq!(dec(&all["closing_balance"]), 16700.0);
    let first = &all["entries"][0];
    assert_eq!(first["posting_date"], "2026-01-10");
    assert_eq!(dec(&first["debit"]), 10000.0);
    assert_eq!(dec(&first["credit"]), 0.0);
    assert_eq!(first["memo"], "Owner invests");
    assert_eq!(first["document_type"], "journal_entry");
    assert_eq!(first["document_number"], "JE-2026-00001");
    assert!(first["posting_id"].is_string());
    assert!(first["document_id"].is_string());
    let rent = &all["entries"][2];
    assert_eq!(dec(&rent["credit"]), 800.0);
    assert_eq!(rent["memo"], Value::Null);
    assert_eq!(rent["posting_memo"], "Rent");
    assert_eq!(rent["document_number"], "JE-2026-00003");

    // From February: January is the opening balance, and the running
    // balance continues from it on both pages.
    let page_1 = ledger(json!({ "from": "2026-02-01", "limit": 2 })).await;
    assert_eq!(dec(&page_1["opening_balance"]), 10000.0);
    assert_eq!(page_1["total"], 3);
    assert_eq!(balances(&page_1), [12500.0, 11700.0]);
    assert_eq!(dec(&page_1["closing_balance"]), 16700.0);
    let page_2 = ledger(json!({ "from": "2026-02-01", "limit": 2, "offset": 2 })).await;
    assert_eq!(dec(&page_2["opening_balance"]), 10000.0);
    assert_eq!(page_2["total"], 3);
    assert_eq!(balances(&page_2), [16700.0]);
    assert_eq!(page_2["entries"][0]["posting_date"], "2026-03-01");
    assert_eq!(dec(&page_2["closing_balance"]), 16700.0);
    // Past the end: no entries, the same figures.
    let page_3 = ledger(json!({ "from": "2026-02-01", "limit": 2, "offset": 4 })).await;
    assert!(balances(&page_3).is_empty());
    assert_eq!(page_3["total"], 3);
    assert_eq!(dec(&page_3["opening_balance"]), 10000.0);
    assert_eq!(dec(&page_3["closing_balance"]), 16700.0);

    // A closed range: the closing balance stops at its last day.
    let february = ledger(json!({ "from": "2026-02-01", "to": "2026-02-28" })).await;
    assert_eq!(february["total"], 2);
    assert_eq!(balances(&february), [12500.0, 11700.0]);
    assert_eq!(dec(&february["closing_balance"]), 11700.0);
    // Only an end date: from the beginning.
    let january = ledger(json!({ "to": "2026-01-31" })).await;
    assert_eq!(dec(&january["opening_balance"]), 0.0);
    assert_eq!(balances(&january), [10000.0]);
    // A range without entries carries the balance through.
    let april = ledger(json!({ "from": "2026-04-01" })).await;
    assert_eq!(april["total"], 0);
    assert!(balances(&april).is_empty());
    assert_eq!(dec(&april["opening_balance"]), 16700.0);
    assert_eq!(dec(&april["closing_balance"]), 16700.0);

    // The limit is capped.
    let capped = ledger(json!({ "limit": 100000 })).await;
    assert_eq!(capped["limit"], 500);

    // A credit account runs negative: the balance is debit minus credit.
    let loan = query(
        &t,
        "gl.account_ledger",
        json!({ "company_id": c.id, "account_id": b.loan }),
    )
    .await;
    assert_eq!(dec(&loan["closing_balance"]), -5000.0);

    let (status, body) = t
        .command(
            &t.admin,
            "gl.account_ledger",
            json!({ "company_id": c.id, "account_id": c.cash, "from": "2026-03-01", "to": "2026-02-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn reports_need_read_access_to_the_company_and_its_accounts() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let other = t.company("OTHER", "USD").await;

    let inputs = |company: &str, account: &str| {
        [
            ("gl.balance_sheet", json!({ "company_id": company })),
            (
                "gl.income_statement",
                json!({ "company_id": company, "from": "2026-01-01", "to": "2026-12-31" }),
            ),
            (
                "gl.account_ledger",
                json!({ "company_id": company, "account_id": account }),
            ),
        ]
    };

    // May read accounts and journals of ACME, but not the ledger.
    let clerk = t
        .user_with_role(
            "clerk@example.com",
            &["account:read", "journal_entry:read"],
            &c.id,
        )
        .await;
    // May read the ledger, but only that of the other company.
    let outsider = t
        .user_with_role("outsider@example.com", &["gl_entry:read"], &other.id)
        .await;
    // May read the ledger of ACME.
    let accountant = t
        .user_with_role("accountant@example.com", &["gl_entry:read"], &c.id)
        .await;

    for (name, input) in inputs(&c.id, &c.cash) {
        for token in [&clerk, &outsider] {
            let (status, body) = t.command(token, name, input.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {body}");
        }
        let (status, body) = t.command(&accountant, name, input).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
    }
    // The outsider's own company is open to them, and empty.
    for (name, input) in inputs(&other.id, &other.cash) {
        let (status, body) = t.command(&outsider, name, input).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body}");
    }
    let sheet = query(&t, "gl.balance_sheet", json!({ "company_id": other.id })).await;
    assert_eq!(dec(&sheet["total_assets"]), 0.0);

    // An account of another company is not found, whichever way round,
    // and so is an account that does not exist.
    for (company, account) in [(&other.id, &c.cash), (&c.id, &other.cash), (&c.id, &c.id)] {
        let (status, body) = t
            .command(
                &t.admin,
                "gl.account_ledger",
                json!({ "company_id": company, "account_id": account }),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
    // Reading the other company's ledger does not open ACME's account.
    let (status, body) = t
        .command(
            &outsider,
            "gl.account_ledger",
            json!({ "company_id": other.id, "account_id": c.cash }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // The reports are published as read-only tools.
    // Every page of them: there are more tools than one page holds.
    let tools = t.all_tools(&t.admin).await;
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for name in [
        "gl_balance_sheet",
        "gl_income_statement",
        "gl_account_ledger",
    ] {
        assert!(names.contains(&name), "{name} is not listed in {names:?}");
    }

    t.stop().await;
}
