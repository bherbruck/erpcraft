//! The audit log: what is recorded, and who may read it.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

fn commands(page: &Value) -> Vec<(&str, &str)> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["command"].as_str().unwrap(),
                e["outcome"].as_str().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_record_has_a_history_including_failed_attempts() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let id = common::id(&draft);
    let path = format!("/api/documents/journal_entry/{id}");

    // A stale edit is refused. Its transaction is rolled back, and the
    // attempt is still on record.
    let (status, _) = t
        .patch(&t.admin, &path, json!({ "version": 99, "memo": "Nope" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": id }))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, history) = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "journal_entry", "id": id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["total"], 3);
    assert_eq!(
        commands(&history),
        [
            ("journal_entry.post", "ok"),
            ("document.update", "error"),
            ("document.create", "ok"),
        ],
        "newest first"
    );
    let failed = &history["items"][1];
    assert_eq!(failed["error_code"], "conflict");
    assert!(
        failed["error_message"]
            .as_str()
            .unwrap()
            .contains("version")
    );
    assert_eq!(failed["user_name"], "Administrator");
    assert_eq!(failed["input"]["memo"], "Nope", "an auditor sees the input");

    // Plain records have a history too.
    let (_, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{}", c.cash),
            json!({ "name": "Till" }),
        )
        .await;
    let (_, history) = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "account", "id": c.cash }),
        )
        .await;
    assert_eq!(
        commands(&history),
        [("entity.update", "ok"), ("entity.create", "ok")]
    );

    // Paging.
    let (_, page) = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "journal_entry", "id": id, "limit": 1, "offset": 2 }),
        )
        .await;
    assert_eq!(commands(&page), [("document.create", "ok")]);
    assert_eq!(page["total"], 3);

    t.stop().await;
}

#[tokio::test]
async fn history_follows_the_right_to_read_the_record() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let other = t.company("OTHER", "USD").await;
    let mine = common::id(&t.draft(&t.admin, &c, "10.00").await);
    let theirs = common::id(&t.draft(&t.admin, &other, "10.00").await);

    let reader = t
        .user_with_role(
            "reader@example.com",
            &["journal_entry:read", "account:read"],
            &c.id,
        )
        .await;

    // Who, what and when, but not the input: it may hold fields this user
    // is not allowed to see.
    let (status, history) = t
        .command(
            &reader,
            "audit.history",
            json!({ "resource": "journal_entry", "id": mine }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(commands(&history), [("document.create", "ok")]);
    assert!(history["items"][0]["input"].is_null());
    assert_eq!(history["items"][0]["user_name"], "Administrator");

    // No history of what they cannot read.
    let (status, _) = t
        .command(
            &reader,
            "audit.history",
            json!({ "resource": "journal_entry", "id": theirs }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .command(
            &reader,
            "audit.history",
            json!({ "resource": "account", "id": other.cash }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Users are not an entity; their history needs the audit permission.
    let (status, _) = t
        .command(
            &reader,
            "audit.history",
            json!({ "resource": "user", "id": mine }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The log as a whole is closed to them.
    let (status, _) = t.command(&reader, "audit.list", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    t.stop().await;
}

#[tokio::test]
async fn the_log_records_refusals_but_not_secrets_previews_or_reads() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let reader = t
        .user_with_role("reader@example.com", &["journal_entry:read"], &c.id)
        .await;
    let before = t.count("SELECT count(*) FROM audit_log").await;

    // A refused attempt to change something is recorded, with what was tried.
    let (status, _) = t
        .post(
            &reader,
            "/api/entities/account",
            json!({ "company_id": c.id, "code": "9", "name": "Mine", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A failed command that handles secrets is recorded without its input.
    let (status, _) = t
        .command(
            &t.admin,
            "user.create",
            json!({ "email": "reader@example.com", "name": "Twin", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A failed preview attempted nothing, and reads are not audited.
    let (status, _) = t
        .post(
            &reader,
            "/api/commands/entity.create?preview=true",
            json!({ "entity": "account", "data": {} }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);
    let (status, _) = t.get(&reader, "/api/entities/account").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    assert_eq!(t.count("SELECT count(*) FROM audit_log").await, before + 2);

    let (status, log) = t
        .command(&t.admin, "audit.list", json!({ "outcome": "error" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert_eq!(log["total"], 2);
    let (secret, refused) = (&log["items"][0], &log["items"][1]);
    assert_eq!(secret["command"], "user.create");
    assert!(secret["input"].is_null());
    assert_eq!(refused["command"], "entity.create");
    assert_eq!(refused["error_code"], "forbidden");
    assert_eq!(refused["input"]["data"]["code"], "9");
    assert_eq!(refused["user_name"], "Test User");

    // Filters.
    let (_, log) = t
        .command(
            &t.admin,
            "audit.list",
            json!({ "command": "entity.create", "outcome": "ok", "limit": 2 }),
        )
        .await;
    assert_eq!(log["items"].as_array().unwrap().len(), 2);
    assert!(log["total"].as_i64().unwrap() > 2);

    // The log cannot be rewritten, even with direct database access.
    let result = sqlx::query("DELETE FROM audit_subject")
        .execute(&t.pool)
        .await;
    assert!(result.is_err());

    t.stop().await;
}
