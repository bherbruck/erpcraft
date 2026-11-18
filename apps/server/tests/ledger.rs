//! The ledger slice, end to end, against a real Postgres.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};
use uuid::Uuid;

async fn entries(t: &TestApp, posting_id: &str) -> Vec<Value> {
    let (status, body) = t
        .get(
            &t.admin,
            &format!("/api/entities/gl_entry?posting_id={posting_id}"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["items"].as_array().unwrap().clone()
}

async fn trial_balance(t: &TestApp, company: &str) -> Value {
    let (status, body) = t
        .command(
            &t.admin,
            "gl.trial_balance",
            json!({ "company_id": company }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Draft a journal, post it through MCP, retry through HTTP, confirm one
/// balanced posting, reverse it, confirm the original is untouched.
#[tokio::test]
async fn post_through_mcp_retry_through_http_then_reverse() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let draft = t.draft(&t.admin, &c, "100.00").await;
    let doc = id(&draft);
    assert_eq!(draft["status"], "draft");
    assert_eq!(draft["number"], Value::Null);

    // MCP exposes the command as a tool with a schema.
    let init = t
        .mcp(
            &t.admin,
            "initialize",
            json!({ "protocolVersion": "2025-06-18" }),
        )
        .await;
    assert_eq!(init["serverInfo"]["name"], "erpcraft");
    let tools = t.all_tools(&t.admin).await;
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "journal_entry_post")
        .expect("journal_entry_post is listed");
    assert_eq!(tool["inputSchema"]["properties"]["id"]["type"], "string");
    assert!(tool["inputSchema"]["properties"]["preview"].is_object());

    // Post through MCP.
    let result = t
        .tool(&t.admin, "journal_entry_post", json!({ "id": doc }))
        .await;
    assert_eq!(result["isError"], false, "{result}");
    let posted = &result["structuredContent"];
    assert_eq!(posted["status"], "posted");
    assert_eq!(posted["number"], "JE-2026-00001");
    assert_eq!(posted["postings"].as_array().unwrap().len(), 1);
    let posting = posted["postings"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(posted["postings"][0]["command"], "journal_entry.post");

    // Retry through HTTP: same outcome, nothing posted twice.
    let (status, again) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["number"], "JE-2026-00001");
    assert_eq!(again["postings"].as_array().unwrap().len(), 1);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 1);
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, 2);
    // Subscribers hear about the change once; the audit log has both calls.
    assert_eq!(
        t.count("SELECT count(*) FROM outbox_event WHERE topic = 'journal_entry.post'")
            .await,
        1
    );
    assert_eq!(
        t.count("SELECT count(*) FROM audit_log WHERE command = 'journal_entry.post'")
            .await,
        2
    );

    // One balanced posting, each entry traceable to its document line.
    let original = entries(&t, &posting).await;
    assert_eq!(original.len(), 2);
    assert_eq!(
        original.iter().map(|e| dec(&e["debit"])).sum::<f64>(),
        100.0
    );
    assert_eq!(
        original.iter().map(|e| dec(&e["credit"])).sum::<f64>(),
        100.0
    );
    let line_ids: Vec<&Value> = posted["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| &l["id"])
        .collect();
    assert!(
        original
            .iter()
            .all(|e| line_ids.contains(&&e["source_line_id"]))
    );

    let tb = trial_balance(&t, &c.id).await;
    assert_eq!(tb["currency"], "USD");
    assert_eq!(dec(&tb["total_debit"]), 100.0);
    assert_eq!(dec(&tb["total_credit"]), 100.0);

    // Reverse: a second posting; the first stays exactly as it was.
    let (status, reversed) = t
        .command(&t.admin, "journal_entry.reverse", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{reversed}");
    assert_eq!(reversed["status"], "reversed");
    assert_eq!(reversed["number"], "JE-2026-00001");
    let postings = reversed["postings"].as_array().unwrap();
    assert_eq!(postings.len(), 2);
    assert_eq!(postings[1]["reverses_id"], posting.as_str());
    assert_eq!(entries(&t, &posting).await, original);
    let reversal = entries(&t, postings[1]["id"].as_str().unwrap()).await;
    assert_eq!(reversal.len(), 2);

    let tb = trial_balance(&t, &c.id).await;
    assert!(
        tb["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| dec(&row["balance"]) == 0.0),
        "{tb}"
    );

    // Reversing again changes nothing.
    let (status, again) = t
        .command(&t.admin, "journal_entry.reverse", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["postings"].as_array().unwrap().len(), 2);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 2);

    t.stop().await;
}

#[tokio::test]
async fn concurrent_posts_of_one_draft_post_exactly_once() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let doc = id(&t.draft(&t.admin, &c, "50.00").await);

    let http = (0..4).map(|_| async {
        let (status, body) = t
            .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["number"].clone()
    });
    let mcp = (0..4).map(|_| async {
        let result = t
            .tool(&t.admin, "journal_entry_post", json!({ "id": doc }))
            .await;
        assert_eq!(result["isError"], false, "{result}");
        result["structuredContent"]["number"].clone()
    });
    let (a, b) = tokio::join!(join_all(http), join_all(mcp));
    assert!(a.iter().chain(&b).all(|n| n == "JE-2026-00001"));

    assert_eq!(t.count("SELECT count(*) FROM posting").await, 1);
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, 2);
    assert_eq!(t.count("SELECT last_value FROM number_series").await, 1);
    assert_eq!(
        t.count("SELECT count(*) FROM outbox_event WHERE topic = 'journal_entry.post'")
            .await,
        1
    );

    t.stop().await;
}

/// Run futures concurrently on the current task.
async fn join_all<F: Future>(futures: impl Iterator<Item = F>) -> Vec<F::Output> {
    // The futures borrow the test app, so they cannot be spawned; poll them
    // together instead.
    let mut pending: Vec<std::pin::Pin<Box<F>>> = futures.map(Box::pin).collect();
    let mut results: Vec<Option<F::Output>> = pending.iter().map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (slot, future) in results.iter_mut().zip(pending.iter_mut()) {
            if slot.is_none() {
                match future.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => *slot = Some(value),
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    results.into_iter().map(Option::unwrap).collect()
}

#[tokio::test]
async fn numbers_stay_gapless_when_a_concurrent_post_fails() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    let mut docs = vec![];
    for _ in 0..5 {
        docs.push(id(&t.draft(&t.admin, &c, "10.00").await));
    }
    // One draft that cannot post: it does not balance.
    let (status, bad) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({
                "company_id": c.id, "doc_date": "2026-03-15",
                "body": { "lines": [
                    { "account_id": c.cash, "debit": "10.00" },
                    { "account_id": c.revenue, "credit": "9.00" },
                ]},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{bad}");
    docs.insert(2, id(&bad));

    let results = join_all(
        docs.iter()
            .map(|doc| t.command(&t.admin, "journal_entry.post", json!({ "id": doc }))),
    )
    .await;
    let mut numbers: Vec<String> = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::OK)
        .map(|(_, body)| body["number"].as_str().unwrap().to_owned())
        .collect();
    numbers.sort();
    let expected: Vec<String> = (1..=5).map(|n| format!("JE-2026-{n:05}")).collect();
    assert_eq!(numbers, expected);

    let failed: Vec<&(StatusCode, Value)> = results
        .iter()
        .filter(|(status, _)| *status != StatusCode::OK)
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        failed[0].1["error"]["message"]
            .as_str()
            .unwrap()
            .contains("do not balance")
    );
    assert_eq!(t.count("SELECT last_value FROM number_series").await, 5);

    t.stop().await;
}

/// A post that starts while a close is in flight waits for it, then is refused.
#[tokio::test]
async fn posting_cannot_race_a_period_close() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let doc = id(&t.draft(&t.admin, &c, "75.00").await);

    let mut closing = t.pool.begin().await.unwrap();
    sqlx::query("UPDATE fiscal_period SET status = 'closed' WHERE id = $1")
        .bind(Uuid::parse_str(&c.period).unwrap())
        .execute(&mut *closing)
        .await
        .unwrap();

    let (status, body) = {
        let post = t.command(&t.admin, "journal_entry.post", json!({ "id": doc }));
        tokio::pin!(post);
        let waited = tokio::time::timeout(Duration::from_millis(500), &mut post).await;
        assert!(
            waited.is_err(),
            "the post must wait for the close to finish"
        );

        closing.commit().await.unwrap();
        post.await
    };
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no open fiscal period")
    );

    let (_, after) = t
        .get(&t.admin, &format!("/api/documents/journal_entry/{doc}"))
        .await;
    assert_eq!(after["status"], "draft");
    assert_eq!(after["number"], Value::Null);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM number_series").await, 0);

    t.stop().await;
}

/// A close that starts while a post is in flight waits for it.
#[tokio::test]
async fn closing_a_period_waits_for_postings_in_flight() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let period = Uuid::parse_str(&c.period).unwrap();

    // The lock every ledger entry takes on its period.
    let mut posting = t.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM fiscal_period WHERE id = $1 FOR SHARE")
        .bind(period)
        .execute(&mut *posting)
        .await
        .unwrap();

    let (status, body) = {
        let close = t.command(&t.admin, "fiscal_period.close", json!({ "id": c.period }));
        tokio::pin!(close);
        let waited = tokio::time::timeout(Duration::from_millis(500), &mut close).await;
        assert!(
            waited.is_err(),
            "the close must wait for the posting to finish"
        );

        posting.commit().await.unwrap();
        close.await
    };
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "closed");

    // Closed means closed, until it is reopened.
    let doc = id(&t.draft(&t.admin, &c, "20.00").await);
    let (status, _) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = t
        .command(&t.admin, "fiscal_period.reopen", json!({ "id": c.period }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_eq!(status, StatusCode::OK);

    // The status cannot be changed around the commands.
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/fiscal_period/{}", c.period),
            json!({ "status": "closed" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    t.stop().await;
}

#[tokio::test]
async fn a_stale_edit_of_a_draft_is_refused() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let path = format!("/api/documents/journal_entry/{}", id(&draft));
    assert_eq!(draft["version"], 1);

    let (status, first) = t
        .patch(
            &t.admin,
            &path,
            json!({ "version": 1, "memo": "First editor" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["version"], 2);
    assert_eq!(first["memo"], "First editor");

    let (status, second) = t
        .patch(
            &t.admin,
            &path,
            json!({ "version": 1, "memo": "Second editor" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{second}");
    let (_, current) = t.get(&t.admin, &path).await;
    assert_eq!(current["memo"], "First editor");

    t.stop().await;
}

#[tokio::test]
async fn an_idempotency_key_replays_the_result_and_rejects_changed_input() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let body = |amount: &str| {
        json!({
            "company_id": c.id, "doc_date": "2026-03-15",
            "body": { "lines": [
                { "account_id": c.cash, "debit": amount },
                { "account_id": c.revenue, "credit": amount },
            ]},
        })
    };
    let key = [("idempotency-key", "sale-42")];
    let create = |amount: &'static str| {
        t.send(
            "POST",
            "/api/documents/journal_entry",
            Some(&t.admin),
            Some(body(amount)),
            &key,
        )
    };

    let (status, first) = create("10.00").await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, replay) = create("10.00").await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(t.count("SELECT count(*) FROM document").await, 1);

    let (status, changed) = create("11.00").await;
    assert_eq!(status, StatusCode::CONFLICT, "{changed}");
    assert_eq!(t.count("SELECT count(*) FROM document").await, 1);

    // Two concurrent first uses of a key create one document.
    let key = [("idempotency-key", "sale-43")];
    let results = join_all((0..4).map(|_| {
        t.send(
            "POST",
            "/api/documents/journal_entry",
            Some(&t.admin),
            Some(body("5.00")),
            &key,
        )
    }))
    .await;
    assert!(
        results
            .iter()
            .all(|(status, _)| *status == StatusCode::CREATED)
    );
    assert!(
        results
            .iter()
            .all(|(_, doc)| doc["id"] == results[0].1["id"])
    );
    assert_eq!(t.count("SELECT count(*) FROM document").await, 2);

    t.stop().await;
}

#[tokio::test]
async fn a_preview_shows_the_outcome_and_saves_nothing() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let doc = id(&t.draft(&t.admin, &c, "30.00").await);

    let (status, preview) = t
        .post(
            &t.admin,
            "/api/commands/journal_entry.post?preview=true",
            json!({ "id": doc }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["result"]["status"], "posted");
    assert_eq!(preview["result"]["number"], "JE-2026-00001");

    let result = t
        .tool(
            &t.admin,
            "journal_entry_post",
            json!({ "id": doc, "preview": true }),
        )
        .await;
    assert_eq!(result["structuredContent"]["preview"], true);
    assert_eq!(
        result["structuredContent"]["result"]["number"],
        "JE-2026-00001"
    );

    let (_, after) = t
        .get(&t.admin, &format!("/api/documents/journal_entry/{doc}"))
        .await;
    assert_eq!(after["status"], "draft");
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM number_series").await, 0);
    assert_eq!(
        t.count("SELECT count(*) FROM audit_log WHERE command = 'journal_entry.post'")
            .await,
        0
    );

    // The real run still gets the first number.
    let (_, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
        .await;
    assert_eq!(posted["number"], "JE-2026-00001");

    t.stop().await;
}

#[tokio::test]
async fn ledger_rules_hold_for_the_api_and_for_raw_sql() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let lines = |a: &str, debit: &str, b: &str, credit: &str| {
        json!({
            "company_id": c.id, "doc_date": "2026-03-15",
            "body": { "lines": [{ "account_id": a, "debit": debit }, { "account_id": b, "credit": credit }] },
        })
    };

    // Out of balance: stays a draft.
    let (_, unbalanced) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            lines(&c.cash, "10.00", &c.revenue, "9.99"),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": unbalanced["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Group accounts cannot be posted to.
    let (_, grouped) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            lines(&c.group, "10.00", &c.revenue, "10.00"),
        )
        .await;
    let (status, body) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": grouped["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("group account"),
        "{body}"
    );

    // No period for the date.
    let mut outside = lines(&c.cash, "10.00", &c.revenue, "10.00");
    outside["doc_date"] = json!("2027-01-01");
    let (_, outside) = t
        .post(&t.admin, "/api/documents/journal_entry", outside)
        .await;
    let (status, _) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": outside["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // More decimals than the currency has.
    let (_, precise) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            lines(&c.cash, "10.005", &c.revenue, "10.005"),
        )
        .await;
    let (status, _) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": precise["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // An account of another company is refused when the draft is saved.
    let other = t.company("OTHER", "USD").await;
    let (status, _) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            lines(&other.cash, "10.00", &c.revenue, "10.00"),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    assert_eq!(t.count("SELECT count(*) FROM posting").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, 0);

    // A posted entry is frozen, through the API ...
    let posted = id(&t.draft(&t.admin, &c, "40.00").await);
    let (status, _) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": posted }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let path = format!("/api/documents/journal_entry/{posted}");
    let (status, _) = t
        .patch(&t.admin, &path, json!({ "version": 2, "memo": "changed" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = t.send("DELETE", &path, Some(&t.admin), None, &[]).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // ... and underneath it.
    for sql in [
        "UPDATE gl_entry SET debit = debit + 1 WHERE debit > 0",
        "DELETE FROM gl_entry",
        "TRUNCATE gl_entry",
        "UPDATE posting SET posting_date = '2026-01-01'",
        "DELETE FROM posting",
        "UPDATE journal_entry_line SET debit = 1 WHERE debit > 0",
        "DELETE FROM journal_entry_line WHERE document_id IN (SELECT id FROM document WHERE locked)",
        "UPDATE document SET memo = 'changed' WHERE locked",
        "DELETE FROM document WHERE locked",
        "UPDATE audit_log SET command = 'x'",
    ] {
        let result = sqlx::query(sql).execute(&t.pool).await;
        assert!(result.is_err(), "the database must refuse: {sql}");
    }

    // An unbalanced posting written around the ledger API fails at commit.
    let mut tx = t.pool.begin().await.unwrap();
    let posting = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
         SELECT $1, company_id, id, 'test', doc_date, created_by FROM document WHERE id = $2",
    )
    .bind(posting)
    .bind(Uuid::parse_str(&posted).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO gl_entry (id, company_id, posting_id, line_no, account_id, posting_date,
            debit, credit, currency, txn_debit, txn_credit, fx_rate)
         SELECT gen_random_uuid(), company_id, id, 1, $2, posting_date, 5, 0, 'USD', 5, 0, 1
         FROM posting WHERE id = $1",
    )
    .bind(posting)
    .bind(Uuid::parse_str(&c.cash).unwrap())
    .execute(&mut *tx)
    .await
    .unwrap();
    let error = tx
        .commit()
        .await
        .expect_err("an unbalanced posting must not commit");
    assert!(error.to_string().contains("does not balance"), "{error}");

    t.stop().await;
}

#[tokio::test]
async fn foreign_amounts_are_converted_at_the_captured_rate() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let entry = |credit: &str, rate: Value| {
        json!({
            "company_id": c.id, "doc_date": "2026-03-15",
            "body": { "lines": [
                { "account_id": c.cash, "debit": "100.00", "currency": "EUR", "fx_rate": rate },
                { "account_id": c.revenue, "credit": credit },
            ]},
        })
    };

    // 100.00 EUR at 1.08375 is 108.375, which rounds to 108.38 USD.
    let (_, doc) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            entry("108.38", json!("1.08375")),
        )
        .await;
    let (status, posted) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc["id"] }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let lines = entries(&t, posted["postings"][0]["id"].as_str().unwrap()).await;
    let euro = lines.iter().find(|e| e["currency"] == "EUR").unwrap();
    assert_eq!(dec(&euro["txn_debit"]), 100.0);
    assert_eq!(dec(&euro["debit"]), 108.38);
    assert_eq!(dec(&euro["fx_rate"]), 1.08375);

    // The books balance in the accounting currency, not the transaction one.
    let (_, doc) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            entry("100.00", json!("1.08375")),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc["id"] }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // A foreign amount needs its rate.
    let (_, doc) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            entry("108.38", Value::Null),
        )
        .await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": doc["id"] }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exchange rate"),
        "{body}"
    );

    t.stop().await;
}

#[tokio::test]
async fn permissions_are_scoped_to_companies_and_tokens() {
    let t = TestApp::start().await;
    let a = t.company("A", "USD").await;
    let b = t.company("B", "USD").await;

    let (status, _) = t
        .send("GET", "/api/entities/account", None, None, &[])
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = t
        .send(
            "POST",
            "/mcp",
            None,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A clerk who may draft and read journals in company A only.
    let clerk = t
        .user_with_role(
            "clerk@example.com",
            &["journal_entry:create", "journal_entry:read", "account:read"],
            &a.id,
        )
        .await;

    let draft = t.draft(&clerk, &a, "10.00").await;
    let (status, _) = t
        .post(
            &clerk,
            "/api/documents/journal_entry",
            json!({ "company_id": b.id, "doc_date": "2026-03-15", "body": { "lines": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Drafting is not posting.
    let (status, _) = t
        .command(&clerk, "journal_entry.post", json!({ "id": draft["id"] }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let result = t
        .tool(&clerk, "journal_entry_post", json!({ "id": draft["id"] }))
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"]["code"], "forbidden");

    // Lists show company A only; company B's records do not exist for the clerk.
    let (status, accounts) = t.get(&clerk, "/api/entities/account").await;
    assert_eq!(status, StatusCode::OK);
    let accounts = accounts["items"].as_array().unwrap();
    assert_eq!(accounts.len(), 3);
    assert!(
        accounts
            .iter()
            .all(|acc| acc["company_id"] == a.id.as_str())
    );
    let (status, _) = t
        .get(&clerk, &format!("/api/entities/account/{}", b.cash))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = t
        .get(
            &clerk,
            &format!("/api/entities/account?company_id={}", b.id),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = t.get(&clerk, "/api/entities/fiscal_period").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .post(
            &clerk,
            "/api/entities/account",
            json!({ "company_id": a.id, "code": "9", "name": "X", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .post(
            &clerk,
            "/api/entities/company",
            json!({ "code": "C", "name": "C", "base_currency": "USD" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The clerk's tool list leaves out what the clerk can never run.
    let tools = t.mcp(&clerk, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"document_create"));
    assert!(!names.contains(&"journal_entry_post"));
    assert!(!names.contains(&"user_create"));

    // A read-only token of the administrator can read but not write.
    let (status, token) = t
        .command(
            &t.admin,
            "auth.create_token",
            json!({ "name": "reporting agent", "scopes": ["*:read"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{token}");
    let agent = token["token"].as_str().unwrap();
    let (status, _) = t.get(agent, "/api/entities/account").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = t
        .post(
            agent,
            "/api/documents/journal_entry",
            json!({ "company_id": a.id, "doc_date": "2026-03-15", "body": { "lines": [] } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .command(agent, "auth.create_token", json!({ "name": "escalate" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Secrets are kept out of the audit log.
    assert_eq!(
        t.count("SELECT count(*) FROM audit_log WHERE command = 'user.create' AND input IS NULL")
            .await,
        1
    );
    assert_eq!(
        t.count("SELECT count(*) FROM audit_log WHERE input::text LIKE '%correct-horse%'")
            .await,
        0
    );

    // A revoked token stops working.
    let (status, _) = t.post(&clerk, "/api/auth/logout", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = t.get(&clerk, "/api/entities/account").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    t.stop().await;
}

#[tokio::test]
async fn the_registry_describes_the_system() {
    let t = TestApp::start().await;

    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    let names = |key: &str| -> Vec<String> {
        meta[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect()
    };
    // The kernel's own module comes first, then the others in the order
    // their dependencies require.
    assert_eq!(
        names("modules")[..4],
        ["kernel", "auth", "gl", "accounting"]
    );
    assert!(names("entities").contains(&"account".to_owned()));
    assert!(names("doc_types").contains(&"journal_entry".to_owned()));
    assert!(names("commands").contains(&"journal_entry.post".to_owned()));
    let journal = meta["doc_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "journal_entry")
        .unwrap();
    assert_eq!(journal["states"].as_array().unwrap().len(), 3);
    assert_eq!(journal["collections"][0]["name"], "lines");

    // One module at a time, for an agent that should not load everything.
    let (_, gl) = t.get(&t.admin, "/api/meta?module=gl").await;
    assert_eq!(gl["modules"].as_array().unwrap().len(), 1);
    assert!(
        gl["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cmd| cmd["module"] == "gl")
    );

    let (status, body) = t.command(&t.admin, "no.such_command", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/company",
            json!({ "code": "X", "name": "X", "base_currency": "ZZZ" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/company",
            json!({ "code": "X", "name": "X", "nope": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_rate_on_record_is_used_when_none_is_stated() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    for (date, rate) in [
        ("2026-01-01", "1.05"),
        ("2026-03-01", "1.10"),
        ("2026-04-01", "1.20"),
    ] {
        t.create(
            "exchange_rate",
            json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": date, "rate": rate }),
        )
        .await;
    }
    let post = |doc_date: &'static str, credit: &'static str, rate: Value| {
        let (t, c) = (&t, &c);
        async move {
            let (status, doc) = t
                .post(
                    &t.admin,
                    "/api/documents/journal_entry",
                    json!({
                        "company_id": c.id, "doc_date": doc_date,
                        "body": { "lines": [
                            { "account_id": c.cash, "debit": "100.00", "currency": "EUR", "fx_rate": rate },
                            { "account_id": c.revenue, "credit": credit },
                        ]},
                    }),
                )
                .await;
            if status != StatusCode::CREATED {
                return (status, doc);
            }
            t.command(&t.admin, "journal_entry.post", json!({ "id": doc["id"] }))
                .await
        }
    };

    // 15 March: the rate that applies is the one from 1 March.
    let (status, posted) = post("2026-03-15", "110.00", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let lines = entries(&t, posted["postings"][0]["id"].as_str().unwrap()).await;
    let euro = lines.iter().find(|e| e["currency"] == "EUR").unwrap();
    assert_eq!(dec(&euro["fx_rate"]), 1.10);
    assert_eq!(dec(&euro["debit"]), 110.0);

    // A rate stated on the line wins over the one on record.
    let (status, posted) = post("2026-03-15", "125.00", json!("1.25")).await;
    assert_eq!(status, StatusCode::OK, "{posted}");

    // Changing the rate on record afterwards changes nothing already posted.
    let (_, rates) = t
        .get(&t.admin, "/api/entities/exchange_rate?rate_date=2026-03-01")
        .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!(
                "/api/entities/exchange_rate/{}",
                common::id(&rates["items"][0])
            ),
            json!({ "rate": "9.99" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lines = entries(&t, posted["postings"][0]["id"].as_str().unwrap()).await;
    assert!(lines.iter().all(|e| dec(&e["fx_rate"]) != 9.99));
    let (_, tb) = t
        .command(&t.admin, "gl.trial_balance", json!({ "company_id": c.id }))
        .await;
    assert_eq!(dec(&tb["total_debit"]), 235.0);

    // A rate finer than the ledger stores is refused rather than rounded.
    let (status, body) = post("2026-03-15", "110.00", json!("1.10000000001")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("at most 10 decimal places"),
        "{body}"
    );

    // Before the first rate on record there is none to use, and the
    // opposite direction is not inferred.
    let (status, body) = post("2025-12-31", "105.00", Value::Null).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("an exchange rate from EUR to USD is required"),
        "{body}"
    );

    t.stop().await;
}

#[tokio::test]
async fn a_reversal_is_listed_after_what_was_recorded_before_it() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    // Several lines each, so that an order left to chance would show.
    let post = |amount: &'static str| {
        let (t, c) = (&t, &c);
        async move {
            let lines: Vec<Value> = (0..4)
                .flat_map(|_| {
                    [
                        json!({ "account_id": c.cash, "debit": amount }),
                        json!({ "account_id": c.revenue, "credit": amount }),
                    ]
                })
                .collect();
            let (status, draft) = t
                .post(
                    &t.admin,
                    "/api/documents/journal_entry",
                    json!({ "company_id": c.id, "doc_date": "2026-03-15", "body": { "lines": lines } }),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{draft}");
            let (status, posted) = t
                .command(&t.admin, "journal_entry.post", json!({ "id": id(&draft) }))
                .await;
            assert_eq!(status, StatusCode::OK, "{posted}");
            posted
        }
    };

    // Everything on one day: first, second, the reversal of the first, third.
    let first = post("10.00").await;
    let second = post("20.00").await;
    let (status, reversed) = t
        .command(
            &t.admin,
            "journal_entry.reverse",
            json!({ "id": id(&first) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reversed}");
    let third = post("30.00").await;
    let reversal = reversed["postings"][1].clone();
    assert_eq!(
        reversal["posting_date"],
        first["postings"][0]["posting_date"]
    );

    // The ledger lists by posting date, then in the order of recording.
    let (status, page) = t
        .get(
            &t.admin,
            &format!("/api/entities/gl_entry?company_id={}", c.id),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let listed: Vec<(&str, i64)> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["posting_id"].as_str().unwrap(),
                e["line_no"].as_i64().unwrap(),
            )
        })
        .collect();
    let expected: Vec<(&str, i64)> = [
        &first["postings"][0],
        &second["postings"][0],
        &reversal,
        &third["postings"][0],
    ]
    .into_iter()
    .flat_map(|posting| (1..=8).map(|line| (posting["id"].as_str().unwrap(), line)))
    .collect();
    assert_eq!(listed, expected);

    t.stop().await;
}
