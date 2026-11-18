//! Document numbers are taken at the end of a command, so a posting that
//! is still at work does not hold up the others of its series.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, id};
use serde_json::{Value, json};

/// A posting in flight does not keep the number series locked.
///
/// The first posting is held up in the middle of its work, where it writes
/// its ledger entries, by a lock this test holds on its fiscal period. A
/// second posting of the same series, in another period, must go through
/// meanwhile. It could not if the first had taken its number before doing
/// its work: the counter would stay locked until the first one finished.
///
/// What is guaranteed about numbers: they are unique and without gaps per
/// company, series and year. Their order follows the order in which the
/// commands finish, closely but not strictly, and not the order in which
/// they started: here the posting that started first gets the later number.
#[tokio::test]
async fn a_posting_in_flight_does_not_hold_up_the_number_series() {
    let t = TestApp::start().await;
    // One company, one year, two fiscal periods: one number series.
    let company = t
        .create(
            "company",
            json!({ "code": "ACME", "name": "ACME Inc", "base_currency": "USD" }),
        )
        .await;
    let mut accounts = vec![];
    for (code, name, kind) in [("1000", "Cash", "asset"), ("4000", "Revenue", "income")] {
        accounts.push(
            t.create(
                "account",
                json!({ "company_id": company, "code": code, "name": name, "account_type": kind }),
            )
            .await,
        );
    }
    let mut periods = vec![];
    for (name, start, end) in [
        ("2026 H1", "2026-01-01", "2026-06-30"),
        ("2026 H2", "2026-07-01", "2026-12-31"),
    ] {
        periods.push(
            t.create(
                "fiscal_period",
                json!({ "company_id": company, "name": name, "start_date": start, "end_date": end }),
            )
            .await,
        );
    }
    let mut drafts = vec![];
    for date in ["2026-03-15", "2026-09-15"] {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/journal_entry",
                json!({
                    "company_id": company,
                    "doc_date": date,
                    "body": { "lines": [
                        { "account_id": accounts[0], "debit": "10.00" },
                        { "account_id": accounts[1], "credit": "10.00" },
                    ]},
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        drafts.push(id(&draft));
    }
    let post = |draft: &str| {
        let (t, draft) = (&t, draft.to_owned());
        async move {
            t.command(&t.admin, "journal_entry.post", json!({ "id": draft }))
                .await
        }
    };

    // Hold the first half-year, as a period close would.
    let mut holder = t.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM fiscal_period WHERE id = $1::uuid FOR UPDATE")
        .bind(&periods[0])
        .execute(&mut *holder)
        .await
        .unwrap();

    // The first posting starts and gets stuck on that lock.
    let mut first = Box::pin(post(&drafts[0]));
    // Every wait below is bounded, so that a failure is a failure and not
    // a test that never ends.
    let limit = Duration::from_secs(20);
    let stuck = async {
        loop {
            let waiting = t
                .count(
                    "SELECT count(*) FROM pg_stat_activity
                     WHERE datname = current_database() AND wait_event_type = 'Lock'",
                )
                .await;
            if waiting > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        result = &mut first => panic!("the first posting should wait for its period: {result:?}"),
        result = tokio::time::timeout(limit, stuck) => {
            result.expect("the first posting never came to wait for its period");
        }
    }
    assert_eq!(
        t.count("SELECT count(*) FROM number_series").await,
        0,
        "no number is taken before the work is done"
    );

    // The second one, in the other half-year, is not held up by it.
    let second: (StatusCode, Value) = tokio::select! {
        result = &mut first => panic!("the first posting should still wait: {result:?}"),
        result = tokio::time::timeout(limit, post(&drafts[1])) => {
            result.expect("the second posting waited for the first one's number")
        }
    };
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    assert_eq!(second.1["number"], "JE-2026-00001");

    // Released, the first one finishes and takes the next number.
    holder.rollback().await.unwrap();
    let (status, body) = tokio::time::timeout(limit, first.as_mut())
        .await
        .expect("the first posting did not finish once its period was released");
    drop(first);
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["number"], "JE-2026-00002");
    assert_eq!(t.count("SELECT last_value FROM number_series").await, 2);

    t.stop().await;
}
