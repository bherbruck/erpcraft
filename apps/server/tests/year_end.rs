//! Year-end close: income and expenses of a fiscal year move to retained
//! earnings, and the reports keep showing each year on its own.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};
use uuid::Uuid;

/// A company with two fiscal years, 2025 and 2026, and postings in both.
struct Books {
    company: Company,
    period_2025: String,
    capital: String,
    retained: String,
    rent: String,
}

async fn account(t: &TestApp, company: &str, code: &str, name: &str, kind: &str) -> String {
    t.create(
        "account",
        json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
    )
    .await
}

/// Draft a two-line journal entry and return its id.
async fn journal_draft(
    t: &TestApp,
    company: &str,
    date: &str,
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
                "body": { "lines": [
                    { "account_id": debit, "debit": amount },
                    { "account_id": credit, "credit": amount },
                ]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    id(&draft)
}

/// Post a two-line journal entry.
async fn journal(t: &TestApp, company: &str, date: &str, debit: &str, credit: &str, amount: &str) {
    let doc = journal_draft(t, company, date, debit, credit, amount).await;
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
}

/// 2025: the owner invests 10000, a sale of 2500, rent of 800: a profit of
/// 1700. 2026: a sale of 400, rent of 1000: a loss of 600. All through cash.
async fn books(t: &TestApp) -> Books {
    let company = t.company("ACME", "USD").await;
    let c = company.id.clone();
    let period_2025 = t
        .create(
            "fiscal_period",
            json!({ "company_id": c, "name": "2025", "start_date": "2025-01-01", "end_date": "2025-12-31" }),
        )
        .await;
    let capital = account(t, &c, "3000", "Owner capital", "equity").await;
    let retained = account(t, &c, "3100", "Retained earnings", "equity").await;
    let rent = account(t, &c, "5000", "Rent", "expense").await;

    let cash = company.cash.clone();
    journal(t, &c, "2025-01-10", &cash, &capital, "10000.00").await;
    journal(t, &c, "2025-06-05", &cash, &company.revenue, "2500.00").await;
    journal(t, &c, "2025-09-20", &rent, &cash, "800.00").await;
    journal(t, &c, "2026-02-01", &cash, &company.revenue, "400.00").await;
    journal(t, &c, "2026-03-01", &rent, &cash, "1000.00").await;
    Books {
        company,
        period_2025,
        capital,
        retained,
        rent,
    }
}

async fn query(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn balance_sheet(t: &TestApp, company: &str, as_of: &str) -> Value {
    let sheet = query(
        t,
        "gl.balance_sheet",
        json!({ "company_id": company, "as_of": as_of }),
    )
    .await;
    assert_eq!(
        dec(&sheet["total_assets"]),
        dec(&sheet["total_liabilities_and_equity"]),
        "{sheet}"
    );
    assert_eq!(
        dec(&sheet["total_liabilities_and_equity"]),
        dec(&sheet["liabilities"]["total"])
            + dec(&sheet["equity"]["total"])
            + dec(&sheet["net_income"]),
        "{sheet}"
    );
    sheet
}

async fn income_statement(t: &TestApp, company: &str, year: &str) -> Value {
    query(
        t,
        "gl.income_statement",
        json!({ "company_id": company, "from": format!("{year}-01-01"), "to": format!("{year}-12-31") }),
    )
    .await
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

/// Debit minus credit of every account that has entries, from the trial
/// balance, as `(account id, balance)`.
async fn trial_balance(t: &TestApp, company: &str) -> Vec<(String, f64)> {
    let body = query(t, "gl.trial_balance", json!({ "company_id": company })).await;
    body["rows"]
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

async fn draft_close(
    t: &TestApp,
    company: &str,
    date: &str,
    period: &str,
    account: &str,
) -> (StatusCode, Value) {
    t.post(
        &t.admin,
        "/api/documents/year_end_close",
        json!({
            "company_id": company,
            "doc_date": date,
            "body": { "fiscal_period_id": period, "retained_earnings_account_id": account },
        }),
    )
    .await
}

/// Draft and post a year-end close; returns the posted document.
async fn close(t: &TestApp, company: &str, date: &str, period: &str, account: &str) -> Value {
    let (status, draft) = draft_close(t, company, date, period, account).await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, posted) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    posted
}

fn message(body: &Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

async fn period_status(t: &TestApp, period: &str) -> String {
    sqlx::query_scalar("SELECT status FROM fiscal_period WHERE id = $1")
        .bind(Uuid::parse_str(period).unwrap())
        .fetch_one(&t.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn closing_a_year_moves_its_result_to_equity() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;

    // Before the close the result of 2025 is not in an equity account.
    let sheet = balance_sheet(&t, &c.id, "2025-12-31").await;
    assert_eq!(rows(&sheet["equity"]), [(b.capital.clone(), 10000.0)]);
    assert_eq!(dec(&sheet["net_income"]), 1700.0);
    assert_eq!(dec(&sheet["total_assets"]), 11700.0);
    // And the result of both years runs together.
    let sheet = balance_sheet(&t, &c.id, "2026-12-31").await;
    assert_eq!(dec(&sheet["net_income"]), 1100.0);

    let posted = close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    assert_eq!(posted["status"], "posted");
    assert!(posted["number"].as_str().unwrap().starts_with("YE"));
    let postings = posted["postings"].as_array().unwrap();
    assert_eq!(postings.len(), 1, "{posted}");
    assert_eq!(postings[0]["command"], "year_end_close.post");
    assert_eq!(postings[0]["posting_date"], "2025-12-31");
    assert_eq!(posted["body"]["closing_posting_id"], postings[0]["id"]);
    assert_eq!(posted["body"]["reversal_posting_id"], Value::Null);
    // One entry per account with a balance, and one for retained earnings.
    assert_eq!(
        t.count("SELECT count(*) FROM gl_entry g JOIN posting p ON p.id = g.posting_id WHERE p.command = 'year_end_close.post'")
            .await,
        3
    );
    // The year is closed for posting as well.
    assert_eq!(period_status(&t, &b.period_2025).await, "closed");
    assert_eq!(period_status(&t, &c.period).await, "open");

    // Posting it again changes nothing.
    let (status, again) = t
        .command(
            &t.admin,
            "year_end_close.post",
            json!({ "id": posted["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["version"], posted["version"]);
    assert_eq!(t.count("SELECT count(*) FROM year_end_closing").await, 1);

    // The last day of 2025: the same total, the result now inside equity.
    let sheet = balance_sheet(&t, &c.id, "2025-12-31").await;
    assert_eq!(
        rows(&sheet["equity"]),
        [(b.capital.clone(), 10000.0), (b.retained.clone(), 1700.0)]
    );
    assert_eq!(dec(&sheet["net_income"]), 0.0);
    assert_eq!(dec(&sheet["total_assets"]), 11700.0);
    // The day before, the year is still running.
    let sheet = balance_sheet(&t, &c.id, "2025-12-30").await;
    assert_eq!(dec(&sheet["net_income"]), 1700.0);

    // The income statement of the closed year still shows the year.
    let statement = income_statement(&t, &c.id, "2025").await;
    assert_eq!(rows(&statement["income"]), [(c.revenue.clone(), 2500.0)]);
    assert_eq!(rows(&statement["expenses"]), [(b.rent.clone(), 800.0)]);
    assert_eq!(dec(&statement["net_income"]), 1700.0);

    // The next year starts from zero, in both reports.
    let statement = income_statement(&t, &c.id, "2026").await;
    assert_eq!(rows(&statement["income"]), [(c.revenue.clone(), 400.0)]);
    assert_eq!(rows(&statement["expenses"]), [(b.rent.clone(), 1000.0)]);
    assert_eq!(dec(&statement["net_income"]), -600.0);
    let sheet = balance_sheet(&t, &c.id, "2026-12-31").await;
    assert_eq!(dec(&sheet["equity"]["total"]), 11700.0);
    assert_eq!(dec(&sheet["net_income"]), -600.0);
    assert_eq!(dec(&sheet["total_assets"]), 11100.0);
    // A range over both years adds them up; closing entries are not in it.
    let both = query(
        &t,
        "gl.income_statement",
        json!({ "company_id": c.id, "from": "2025-01-01", "to": "2026-12-31" }),
    )
    .await;
    assert_eq!(dec(&both["net_income"]), 1100.0);

    // Nothing more goes into the closed year.
    let late = journal_draft(&t, &c.id, "2025-12-15", &c.cash, &c.revenue, "50.00").await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": late }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("no open fiscal period"), "{body}");

    // 2026 is a loss year: retained earnings are debited.
    let posted = close(&t, &c.id, "2026-12-31", &c.period, &b.retained).await;
    let retained: (f64, f64) = sqlx::query_as(
        "SELECT debit::float8, credit::float8 FROM gl_entry WHERE posting_id = $1 AND account_id = $2",
    )
    .bind(Uuid::parse_str(posted["postings"][0]["id"].as_str().unwrap()).unwrap())
    .bind(Uuid::parse_str(&b.retained).unwrap())
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(retained, (600.0, 0.0));

    let sheet = balance_sheet(&t, &c.id, "2026-12-31").await;
    assert_eq!(
        rows(&sheet["equity"]),
        [(b.capital.clone(), 10000.0), (b.retained.clone(), 1100.0)]
    );
    assert_eq!(dec(&sheet["net_income"]), 0.0);
    assert_eq!(dec(&sheet["total_assets"]), 11100.0);
    let statement = income_statement(&t, &c.id, "2026").await;
    assert_eq!(dec(&statement["net_income"]), -600.0);
    // The first close is untouched by the second.
    let sheet = balance_sheet(&t, &c.id, "2025-12-31").await;
    assert_eq!(dec(&sheet["equity"]["total"]), 11700.0);
    assert_eq!(dec(&sheet["net_income"]), 0.0);

    t.stop().await;
}

#[tokio::test]
async fn a_year_end_close_that_is_wrong_is_refused() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let other = t.company("OTHER", "USD").await;
    let other_equity = account(&t, &other.id, "3100", "Retained earnings", "equity").await;
    let equity_group = t
        .create(
            "account",
            json!({ "company_id": c.id, "code": "3", "name": "Equity", "account_type": "equity", "is_group": true }),
        )
        .await;

    // Not the last day of the period.
    let (status, body) = draft_close(&t, &c.id, "2025-12-30", &b.period_2025, &b.retained).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("2025-12-31"), "{body}");
    // The last day of another period.
    let (status, body) = draft_close(&t, &c.id, "2026-12-31", &b.period_2025, &b.retained).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Not an equity account.
    let (status, body) = draft_close(&t, &c.id, "2025-12-31", &b.period_2025, &c.cash).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("equity account"), "{body}");
    // An account of another company.
    let (status, body) = draft_close(&t, &c.id, "2025-12-31", &b.period_2025, &other_equity).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("document's company"), "{body}");
    // A group account.
    let (status, body) = draft_close(&t, &c.id, "2025-12-31", &b.period_2025, &equity_group).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("group account"), "{body}");
    // A period of another company.
    let (status, body) = draft_close(&t, &c.id, "2026-12-31", &other.period, &b.retained).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("fiscal period"), "{body}");
    assert_eq!(t.count("SELECT count(*) FROM year_end_close").await, 0);

    // A draft whose date is moved afterwards is caught when it is posted.
    let (status, draft) = draft_close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let path = format!("/api/documents/year_end_close/{}", id(&draft));
    let (status, moved) = t
        .patch(
            &t.admin,
            &path,
            json!({ "version": 1, "doc_date": "2025-11-30" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let (status, body) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("last day"), "{body}");
    assert_eq!(period_status(&t, &b.period_2025).await, "open");

    // Two drafts for one period: the first to post wins.
    let first = close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    let (status, second) = draft_close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let (status, body) = t
        .command(
            &t.admin,
            "year_end_close.post",
            json!({ "id": id(&second) }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        message(&body).contains(first["number"].as_str().unwrap()),
        "{body}"
    );
    // Reopening the period does not make room for a second close either.
    let (status, _) = t
        .command(
            &t.admin,
            "fiscal_period.reopen",
            json!({ "id": b.period_2025 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = t
        .command(
            &t.admin,
            "year_end_close.post",
            json!({ "id": id(&second) }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // The database says the same to anyone who goes around the command.
    let forced = sqlx::query(
        "INSERT INTO year_end_closing (document_id, fiscal_period_id, posting_id)
         SELECT $1, fiscal_period_id, posting_id FROM year_end_closing",
    )
    .bind(Uuid::parse_str(&id(&second)).unwrap())
    .execute(&t.pool)
    .await;
    assert!(forced.is_err());
    let erased = sqlx::query("DELETE FROM year_end_closing")
        .execute(&t.pool)
        .await;
    assert!(erased.is_err());
    assert_eq!(t.count("SELECT count(*) FROM year_end_closing").await, 1);
    assert_eq!(
        t.count("SELECT count(*) FROM posting WHERE command = 'year_end_close.post'")
            .await,
        1
    );

    // A year without income or expenses has nothing to close.
    let (status, empty) =
        draft_close(&t, &other.id, "2026-12-31", &other.period, &other_equity).await;
    assert_eq!(status, StatusCode::CREATED, "{empty}");
    let (status, body) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&empty) }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(message(&body).contains("nothing to close"), "{body}");
    assert_eq!(period_status(&t, &other.period).await, "open");
    let (_, empty) = t
        .get(
            &t.admin,
            &format!("/api/documents/year_end_close/{}", id(&empty)),
        )
        .await;
    assert_eq!(empty["status"], "draft");
    assert_eq!(empty["number"], Value::Null);

    // A period that is closed already cannot take the closing entries.
    journal(
        &t,
        &other.id,
        "2026-05-01",
        &other.cash,
        &other.revenue,
        "10.00",
    )
    .await;
    let (status, _) = t
        .command(
            &t.admin,
            "fiscal_period.close",
            json!({ "id": other.period }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&empty) }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(message(&body).contains("fiscal_period.reopen"), "{body}");

    t.stop().await;
}

#[tokio::test]
async fn reversing_a_year_end_close_restores_every_balance() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let before = trial_balance(&t, &c.id).await;

    let posted = close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    let doc = id(&posted);
    assert_ne!(trial_balance(&t, &c.id).await, before);

    // The closed period refuses the reversal, and says what to do.
    let (status, body) = t
        .command(&t.admin, "year_end_close.reverse", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(message(&body).contains("fiscal_period.reopen"), "{body}");

    let (status, _) = t
        .command(
            &t.admin,
            "fiscal_period.reopen",
            json!({ "id": b.period_2025 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, reversed) = t
        .command(&t.admin, "year_end_close.reverse", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{reversed}");
    assert_eq!(reversed["status"], "reversed");
    let postings = reversed["postings"].as_array().unwrap();
    assert_eq!(postings.len(), 2, "{reversed}");
    assert_eq!(postings[1]["command"], "year_end_close.reverse");
    assert_eq!(postings[1]["posting_date"], "2025-12-31");
    assert_eq!(postings[1]["reverses_id"], postings[0]["id"]);
    assert_eq!(reversed["body"]["reversal_posting_id"], postings[1]["id"]);

    // Reversing it again changes nothing.
    let (status, again) = t
        .command(&t.admin, "year_end_close.reverse", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["version"], reversed["version"]);
    assert_eq!(again["postings"].as_array().unwrap().len(), 2);

    // Every account is where it was, and the reports show an open year.
    assert_eq!(trial_balance(&t, &c.id).await, before);
    let sheet = balance_sheet(&t, &c.id, "2025-12-31").await;
    assert_eq!(rows(&sheet["equity"]), [(b.capital.clone(), 10000.0)]);
    assert_eq!(dec(&sheet["net_income"]), 1700.0);
    let statement = income_statement(&t, &c.id, "2025").await;
    assert_eq!(dec(&statement["net_income"]), 1700.0);
    // The period stays open and takes entries again.
    assert_eq!(period_status(&t, &b.period_2025).await, "open");
    journal(&t, &c.id, "2025-12-20", &b.rent, &c.cash, "200.00").await;

    // The year can be closed again, with what was added since.
    close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    let sheet = balance_sheet(&t, &c.id, "2025-12-31").await;
    assert_eq!(
        rows(&sheet["equity"]),
        [(b.capital.clone(), 10000.0), (b.retained.clone(), 1500.0)]
    );
    assert_eq!(dec(&sheet["net_income"]), 0.0);
    let statement = income_statement(&t, &c.id, "2025").await;
    assert_eq!(rows(&statement["expenses"]), [(b.rent.clone(), 1000.0)]);
    assert_eq!(dec(&statement["net_income"]), 1500.0);

    t.stop().await;
}

/// The close reads the balances only once it has the period to itself.
#[tokio::test]
async fn a_year_end_close_waits_for_postings_in_flight_and_includes_them() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let (status, draft) = draft_close(&t, &c.id, "2025-12-31", &b.period_2025, &b.retained).await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");

    // A posting of 300 rent into 2025 that has written its entries and not
    // yet committed. Each entry takes its period's row FOR SHARE.
    let late = journal_draft(&t, &c.id, "2025-12-15", &b.rent, &c.cash, "300.00").await;
    let mut in_flight = t.pool.begin().await.unwrap();
    let posting: Uuid = sqlx::query_scalar(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
         SELECT gen_random_uuid(), company_id, id, 'journal_entry.post', doc_date, created_by
         FROM document WHERE id = $1 RETURNING id",
    )
    .bind(Uuid::parse_str(&late).unwrap())
    .fetch_one(&mut *in_flight)
    .await
    .unwrap();
    for (line_no, account, debit, credit) in [(1, &b.rent, 300, 0), (2, &c.cash, 0, 300)] {
        sqlx::query(
            "INSERT INTO gl_entry (id, company_id, posting_id, line_no, account_id, posting_date,
                debit, credit, currency, txn_debit, txn_credit, fx_rate)
             SELECT gen_random_uuid(), company_id, id, $2, $3, posting_date, $4, $5, 'USD', $4, $5, 1
             FROM posting WHERE id = $1",
        )
        .bind(posting)
        .bind(line_no)
        .bind(Uuid::parse_str(account).unwrap())
        .bind(debit)
        .bind(credit)
        .execute(&mut *in_flight)
        .await
        .unwrap();
    }

    let (status, posted) = {
        let close = t.command(&t.admin, "year_end_close.post", json!({ "id": id(&draft) }));
        tokio::pin!(close);
        let waited = tokio::time::timeout(Duration::from_millis(500), &mut close).await;
        assert!(
            waited.is_err(),
            "the close must wait for the posting to finish"
        );
        in_flight.commit().await.unwrap();
        close.await
    };
    assert_eq!(status, StatusCode::OK, "{posted}");

    // The rent that committed while the close waited is closed with the rest.
    let sheet = balance_sheet(&t, &c.id, "2025-12-31").await;
    assert_eq!(dec(&sheet["net_income"]), 0.0);
    assert_eq!(
        rows(&sheet["equity"]),
        [(b.capital.clone(), 10000.0), (b.retained.clone(), 1400.0)]
    );

    t.stop().await;
}

#[tokio::test]
async fn closing_a_year_needs_its_own_permissions() {
    let t = TestApp::start().await;
    let b = books(&t).await;
    let c = &b.company;
    let clerk = t
        .user_with_role(
            "clerk@example.com",
            &[
                "year_end_close:create",
                "year_end_close:read",
                "journal_entry:post",
                "fiscal_period:close",
            ],
            &c.id,
        )
        .await;
    let closer = t
        .user_with_role(
            "closer@example.com",
            &["year_end_close:read", "year_end_close:post"],
            &c.id,
        )
        .await;

    // Drafting is not posting.
    let (status, draft) = t
        .post(
            &clerk,
            "/api/documents/year_end_close",
            json!({
                "company_id": c.id,
                "doc_date": "2025-12-31",
                "body": { "fiscal_period_id": b.period_2025, "retained_earnings_account_id": b.retained },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, body) = t
        .command(&clerk, "year_end_close.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(period_status(&t, &b.period_2025).await, "open");

    // The permission to post is enough to close the period with it.
    let (status, body) = t
        .command(&closer, "year_end_close.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(period_status(&t, &b.period_2025).await, "closed");

    // Posting is not reversing.
    let (status, _) = t
        .command(
            &t.admin,
            "fiscal_period.reopen",
            json!({ "id": b.period_2025 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = t
        .command(
            &closer,
            "year_end_close.reverse",
            json!({ "id": id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM posting WHERE command = 'year_end_close.reverse'")
            .await,
        0
    );

    t.stop().await;
}

/// A year kept as twelve monthly periods is closed in one step: the
/// income and expenses of all twelve go to retained earnings, dated the
/// last day of December, and every month is closed.
#[tokio::test]
async fn a_year_of_monthly_periods_is_closed_in_one_step() {
    let t = TestApp::start().await;
    let company = t.company("ACME", "USD").await;
    let c = company.id.clone();
    let mut months = vec![];
    let last_day = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for month in 1..=12usize {
        let start = format!("2025-{month:02}-01");
        let end = format!("2025-{month:02}-{}", last_day[month - 1]);
        months.push(
            t.create(
                "fiscal_period",
                json!({ "company_id": c, "name": format!("2025-{month:02}"),
                        "start_date": start, "end_date": end }),
            )
            .await,
        );
    }
    let retained = account(&t, &c, "3100", "Retained earnings", "equity").await;
    let rent = account(&t, &c, "5000", "Rent", "expense").await;
    let cash = company.cash.clone();
    journal(&t, &c, "2025-03-05", &cash, &company.revenue, "2500.00").await;
    journal(&t, &c, "2025-11-20", &rent, &cash, "800.00").await;
    // March is closed already, as a month-end close leaves it.
    let (status, body) = t
        .command(&t.admin, "fiscal_period.close", json!({ "id": months[2] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The first period must not come after the last.
    let (status, refused) = t
        .post(
            &t.admin,
            "/api/documents/year_end_close",
            json!({ "company_id": c, "doc_date": "2025-01-31", "body": {
                "fiscal_period_id": months[0], "first_period_id": months[11],
                "retained_earnings_account_id": retained } }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["error"]["field"], "first_period_id", "{refused}");

    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/year_end_close",
            json!({ "company_id": c, "doc_date": "2025-12-31", "body": {
                "fiscal_period_id": months[11], "first_period_id": months[0],
                "retained_earnings_account_id": retained } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    let (status, posted) = t
        .command(&t.admin, "year_end_close.post", json!({ "id": id(&draft) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    assert_eq!(dec(&posted["summary"]["result"]), 1700.0, "{posted}");
    assert_eq!(posted["body"]["first_period_id"], months[0].as_str());
    for month in &months {
        assert_eq!(period_status(&t, month).await, "closed");
    }
    // Income and expenses are zero for the year; retained earnings has it.
    let balances = trial_balance(&t, &c).await;
    let at = |account: &str| {
        balances
            .iter()
            .find(|(a, _)| a == account)
            .map_or(0.0, |(_, b)| *b)
    };
    assert_eq!(at(&company.revenue), 0.0);
    assert_eq!(at(&rent), 0.0);
    assert_eq!(at(&retained), -1700.0);
    // Nothing more goes into the year, in any month.
    let doc = journal_draft(&t, &c, "2025-06-10", &rent, &cash, "1.00").await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_ne!(status, StatusCode::OK, "{body}");

    // June reopened and closed again on its own, or with the half year:
    // the close of the whole year covers it still, so it is refused, and
    // June's income is not moved twice.
    let (status, body) = t
        .command(&t.admin, "fiscal_period.reopen", json!({ "id": months[5] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let year_close = id(&posted);
    for first in [None, Some(&months[0])] {
        let mut body = json!({ "fiscal_period_id": months[5],
                               "retained_earnings_account_id": retained });
        if let Some(first) = first {
            body["first_period_id"] = json!(first);
        }
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/year_end_close",
                json!({ "company_id": c, "doc_date": "2025-06-30", "body": body }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let (status, refused) = t
            .command(&t.admin, "year_end_close.post", json!({ "id": id(&draft) }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{refused}");
        assert!(
            message(&refused).contains("already closes fiscal period 2025-01 to 2025-12")
                && message(&refused).contains("year_end_close.reverse"),
            "{refused}"
        );
    }
    assert_eq!(at_now(&t, &c, &retained).await, -1700.0);
    assert_eq!(at_now(&t, &c, &company.revenue).await, 0.0);

    // Once the year's close is reversed, June can be closed on its own:
    // its income moves once.
    let (status, body) = t
        .command(&t.admin, "fiscal_period.reopen", json!({ "id": months[11] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(&t.admin, "year_end_close.reverse", json!({ "id": year_close }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(at_now(&t, &c, &retained).await, 0.0);
    journal(&t, &c, "2025-06-05", &cash, &company.revenue, "1000.00").await;
    let june = close(&t, &c, "2025-06-30", &months[5], &retained).await;
    assert_eq!(dec(&june["summary"]["result"]), 1000.0, "{june}");
    assert_eq!(at_now(&t, &c, &retained).await, -1000.0);
    t.stop().await;
}

/// An account's balance in the trial balance now.
async fn at_now(t: &TestApp, company: &str, account: &str) -> f64 {
    trial_balance(t, company)
        .await
        .iter()
        .find(|(a, _)| a == account)
        .map_or(0.0, |(_, b)| *b)
}
