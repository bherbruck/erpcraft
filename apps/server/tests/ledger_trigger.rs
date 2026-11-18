//! The general ledger's rules are checked once per statement. These tests
//! write entries with raw SQL, several postings to a statement, to show
//! that a bad row among good ones is caught and named, and that entries
//! and the close of their period still wait for each other.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{Company, TestApp, id};
use serde_json::{Value, json};
use uuid::Uuid;

const INSERT: &str = "INSERT INTO gl_entry (id, company_id, posting_id, line_no, account_id,
        posting_date, debit, credit, currency, txn_debit, txn_credit, fx_rate)
    SELECT gen_random_uuid(), r.company_id, r.posting_id, r.line_no, r.account_id,
        r.posting_date, r.debit, r.credit, r.currency, r.txn_debit, r.txn_credit, r.fx_rate
    FROM jsonb_to_recordset($1) AS r(company_id uuid, posting_id uuid, line_no int,
        account_id uuid, posting_date date, debit numeric, credit numeric, currency text,
        txn_debit numeric, txn_credit numeric, fx_rate numeric)";

/// A posting of `company` with no entries yet, under a document of its own.
async fn posting(t: &TestApp, company: &Company, date: &str) -> Uuid {
    let draft = t.draft(&t.admin, company, "1.00").await;
    let posting = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
         SELECT $1, company_id, id, 'test', $3::date, created_by FROM document WHERE id = $2",
    )
    .bind(posting)
    .bind(Uuid::parse_str(&id(&draft)).unwrap())
    .bind(date)
    .execute(&t.pool)
    .await
    .unwrap();
    posting
}

/// Two entries that balance: cash debited, revenue credited, 5.00 USD.
fn pair(company: &Company, posting: Uuid, date: &str) -> Vec<Value> {
    let entry = |line_no: i32, account: &str, debit: &str, credit: &str| {
        json!({
            "company_id": company.id, "posting_id": posting, "line_no": line_no,
            "account_id": account, "posting_date": date, "currency": "USD", "fx_rate": "1",
            "debit": debit, "credit": credit, "txn_debit": debit, "txn_credit": credit,
        })
    };
    vec![
        entry(1, &company.cash, "5.00", "0"),
        entry(2, &company.revenue, "0", "5.00"),
    ]
}

/// Write the rows in one statement; the database's answer if it refuses.
async fn write(t: &TestApp, rows: &[Value]) -> Result<(), String> {
    sqlx::query(INSERT)
        .bind(json!(rows))
        .execute(&t.pool)
        .await
        .map(|_| ())
        .map_err(|error| match error.as_database_error() {
            Some(db) => db.message().to_owned(),
            None => error.to_string(),
        })
}

#[tokio::test]
async fn one_bad_row_among_good_ones_fails_the_statement_and_is_named() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let other = t.company("OTHER", "USD").await;
    let account = |company: &Company, code: &'static str| {
        let (t, company_id) = (&t, company.id.clone());
        async move {
            t.create(
                "account",
                json!({ "company_id": company_id, "code": code, "name": code, "account_type": "asset" }),
            )
            .await
        }
    };
    let theirs = account(&other, "7777").await;
    let retired = account(&c, "1999").await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{retired}"),
            json!({ "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let day = "2026-03-15";
    let (first, second) = (posting(&t, &c, day).await, posting(&t, &c, day).await);
    let before = t.count("SELECT count(*) FROM gl_entry").await;
    // Two postings of two good entries each, with one field of the last
    // entry changed.
    let spoiled = |changes: Value| {
        let mut rows = pair(&c, first, day);
        rows.extend(pair(&c, second, day));
        for (key, value) in changes.as_object().unwrap() {
            rows[3][key] = value.clone();
        }
        rows
    };
    let at = format!("(line 2 of posting {second})");

    for (changes, why) in [
        (
            json!({ "company_id": other.id }),
            format!("a ledger entry must carry the company and date of its posting {at}"),
        ),
        (
            json!({ "posting_date": "2026-03-16" }),
            format!("a ledger entry must carry the company and date of its posting {at}"),
        ),
        (
            json!({ "account_id": theirs }),
            "account 7777 belongs to another company".to_owned(),
        ),
        (
            json!({ "account_id": c.group }),
            "account 1 is a group account and cannot be posted to".to_owned(),
        ),
        (
            json!({ "account_id": retired }),
            "account 1999 is inactive".to_owned(),
        ),
        (
            json!({ "credit": "5.005", "txn_credit": "5.005" }),
            format!("an amount has more decimal places than USD allows {at}"),
        ),
        (
            json!({ "currency": "JPY", "fx_rate": "0.01", "txn_credit": "500.5", "credit": "5.01" }),
            format!("an amount has more decimal places than JPY allows {at}"),
        ),
        (
            json!({ "fx_rate": "1.5", "credit": "7.50" }),
            format!("an entry in the accounting currency must have an exchange rate of 1 {at}"),
        ),
        (
            json!({ "credit": "5.01" }),
            format!(
                "the accounting amount must equal the transaction amount times the exchange \
                 rate, rounded to 2 decimals {at}"
            ),
        ),
        (
            json!({ "currency": "EUR", "fx_rate": "1.105", "txn_credit": "4.52", "credit": "5.00" }),
            // 4.52 times 1.105 is 4.9946: 4.99, not 5.00.
            format!(
                "the accounting amount must equal the transaction amount times the exchange \
                 rate, rounded to 2 decimals {at}"
            ),
        ),
    ] {
        let refused = write(&t, &spoiled(changes.clone())).await;
        assert_eq!(refused, Err(why), "{changes}");
        assert_eq!(
            t.count("SELECT count(*) FROM gl_entry").await,
            before,
            "the good rows of a refused statement are not kept: {changes}"
        );
    }

    // Of several bad rows the first, by posting and line, is reported.
    let mut rows = spoiled(json!({ "account_id": retired }));
    rows[1]["account_id"] = json!(c.group);
    assert_eq!(
        write(&t, &rows).await,
        Err("account 1 is a group account and cannot be posted to".into())
    );
    // The order of the rows in the statement does not matter.
    rows.reverse();
    assert_eq!(
        write(&t, &rows).await,
        Err("account 1 is a group account and cannot be posted to".into())
    );

    // A posting dated outside every open period, beside one that is fine.
    let late = "2027-01-10";
    let outside = posting(&t, &c, late).await;
    let mut rows = pair(&c, first, day);
    rows.extend(pair(&c, outside, late));
    assert_eq!(
        write(&t, &rows).await,
        Err("no open fiscal period contains 2027-01-10".into())
    );
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, before);

    // The same statements without the bad row go in: two postings, and an
    // entry in a foreign currency converted and rounded as the rule says.
    assert_eq!(write(&t, &spoiled(json!({}))).await, Ok(()));
    let third = posting(&t, &c, day).await;
    let mut rows = pair(&c, third, day);
    for (key, value) in
        json!({ "currency": "EUR", "fx_rate": "1.105", "txn_credit": "4.52", "credit": "4.99" })
            .as_object()
            .unwrap()
    {
        rows[1][key] = value.clone();
    }
    rows[0]["debit"] = json!("4.99");
    rows[0]["txn_debit"] = json!("4.99");
    assert_eq!(write(&t, &rows).await, Ok(()));
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, before + 6);
    // A statement that inserts nothing is nothing to object to.
    assert_eq!(write(&t, &[]).await, Ok(()));

    // What balances is still decided when the transaction commits.
    let fourth = posting(&t, &c, day).await;
    let refused = write(&t, &pair(&c, fourth, day)[..1]).await;
    assert!(refused.unwrap_err().contains("does not balance"));

    t.stop().await;
}

/// The lock on the period is taken by the entries themselves, not only by
/// the posting they hang on: entries added to a posting that was written
/// earlier, in another transaction, wait for a close in flight and then
/// see it; and a close waits for entries that are not committed yet.
#[tokio::test]
async fn entries_and_the_close_of_their_period_wait_for_each_other() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let day = "2026-03-15";
    let period = Uuid::parse_str(&c.period).unwrap();
    // Committed before anything below begins.
    let older = posting(&t, &c, day).await;
    let rows = pair(&c, older, day);

    // A close is in flight: the entries wait, and are refused once it
    // commits.
    let mut closing = t.pool.begin().await.unwrap();
    sqlx::query("UPDATE fiscal_period SET status = 'closed' WHERE id = $1")
        .bind(period)
        .execute(&mut *closing)
        .await
        .unwrap();
    let refused = {
        let writing = write(&t, &rows);
        tokio::pin!(writing);
        let waited = tokio::time::timeout(Duration::from_millis(500), &mut writing).await;
        assert!(waited.is_err(), "the entries must wait for the close");
        closing.commit().await.unwrap();
        writing.await
    };
    assert_eq!(
        refused,
        Err("no open fiscal period contains 2026-03-15".into())
    );
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, 0);

    // The other way round: entries are written and not yet committed, so
    // a close waits for them, then goes through.
    let (status, body) = t
        .command(&t.admin, "fiscal_period.reopen", json!({ "id": c.period }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut writing = t.pool.begin().await.unwrap();
    sqlx::query(INSERT)
        .bind(json!(rows))
        .execute(&mut *writing)
        .await
        .unwrap();
    let (status, body) = {
        let close = t.command(&t.admin, "fiscal_period.close", json!({ "id": c.period }));
        tokio::pin!(close);
        let waited = tokio::time::timeout(Duration::from_millis(500), &mut close).await;
        assert!(waited.is_err(), "the close must wait for the entries");
        writing.commit().await.unwrap();
        close.await
    };
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "closed");
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, 2);

    t.stop().await;
}
