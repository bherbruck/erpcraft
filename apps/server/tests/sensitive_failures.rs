//! A command that handles secrets and fails must not write down what it was
//! sent: a secret pasted into the wrong field, or sent as the wrong type,
//! appears in no table and in no answer beyond what the caller sent.

mod common;

use common::TestApp;
use serde_json::{Value, json};

/// Pasted where it does not belong.
const SECRET: &str = "s3cr3t-pasted-in-the-wrong-field";
/// The same as a number, for fields that take text.
const NUMBER: u64 = 731_958_264_017;

/// Where a failed command could leave a trace.
async fn recorded(t: &TestApp) -> String {
    sqlx::query_scalar(
        "SELECT concat_ws(' ',
            (SELECT string_agg(a::text, ' ') FROM audit_log a),
            (SELECT string_agg(o::text, ' ') FROM outbox_event o),
            (SELECT string_agg(k::text, ' ') FROM idempotency_key k),
            (SELECT string_agg(e::text, ' ') FROM auth_event e))",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_failed_sensitive_command_records_nothing_it_was_sent() {
    let t = TestApp::start().await;

    // Something to point the commands on one record at.
    let (status, hook) = t
        .command(
            &t.admin,
            "webhook.create",
            json!({ "name": "hook", "url": "https://example.com/hook" }),
        )
        .await;
    // Without a network the name may not be found; then there is no
    // webhook, and the commands on one fail as not found, which is a
    // failure all the same.
    let hook_id = match status.as_u16() {
        200 => hook["id"].as_str().unwrap().to_owned(),
        _ => uuid::Uuid::new_v4().to_string(),
    };
    let nobody = uuid::Uuid::new_v4().to_string();
    let provider = |more: Value| {
        let mut input = json!({
            "name": "Identity", "slug": "idp", "issuer": "https://idp.example.com",
            "client_id": "client", "client_secret": "a-client-secret-of-some-length",
            "allowed_domains": ["example.com"],
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        input
    };

    let cases: Vec<(&str, Value)> = vec![
        // The wrong type, in a field of another type.
        (
            "user.create",
            json!({ "email": "a@example.com", "name": "A", "is_admin": SECRET }),
        ),
        (
            "user.create",
            json!({ "email": "a@example.com", "name": "A", "password": [SECRET] }),
        ),
        (
            "user.create",
            json!({ "email": "a@example.com", "name": "A", "password": NUMBER }),
        ),
        // In a field of the right type, but the input fails on something else.
        ("user.create", json!({ "email": SECRET })),
        (
            "auth.create_token",
            json!({ "name": "t", "expires_in_days": SECRET }),
        ),
        (
            "auth.create_token",
            json!({ "name": SECRET, "scopes": SECRET }),
        ),
        ("auth.create_token", json!({ "name": NUMBER })),
        (
            "webhook.create",
            json!({ "name": "h", "url": "https://example.com/", "secret": NUMBER }),
        ),
        ("webhook.create", json!({ "name": "h", "url": SECRET })),
        (
            "webhook.create",
            json!({ "name": "h", "url": "https://example.com/", "topics": SECRET }),
        ),
        (
            "webhook.create",
            json!({ "name": SECRET, "url": "https://[::1]/", "secret": "short" }),
        ),
        ("webhook.update", json!({ "id": hook_id, "url": SECRET })),
        ("webhook.update", json!({ "id": hook_id, "active": SECRET })),
        ("webhook.update", json!({ "id": SECRET })),
        (
            "webhook.update",
            json!({ "id": nobody, "name": SECRET, "topics": [SECRET] }),
        ),
        (
            "webhook.set_secret",
            json!({ "id": hook_id, "secret": NUMBER }),
        ),
        (
            "webhook.set_secret",
            json!({ "id": hook_id, "secret": [SECRET] }),
        ),
        ("webhook.set_secret", json!({ "id": SECRET })),
        (
            "webhook.set_secret",
            json!({ "id": nobody, "secret": SECRET }),
        ),
        (
            "sso_provider.create",
            provider(json!({ "client_secret": [SECRET] })),
        ),
        (
            "sso_provider.create",
            provider(json!({ "client_secret": NUMBER })),
        ),
        (
            "sso_provider.create",
            provider(json!({ "slug": format!("Not A Slug {SECRET}") })),
        ),
        (
            "sso_provider.create",
            provider(json!({ "allowed_domains": SECRET })),
        ),
        (
            "sso_provider.set_secret",
            json!({ "id": nobody, "client_secret": { "value": SECRET } }),
        ),
        (
            "sso_provider.set_secret",
            json!({ "id": SECRET, "client_secret": "x" }),
        ),
        (
            "sso_provider.set_secret",
            json!({ "id": nobody, "client_secret": SECRET }),
        ),
    ];

    let number = NUMBER.to_string();
    for (n, (command, input)) in cases.iter().enumerate() {
        let key = format!("key-{n}");
        let (status, body) = t
            .send(
                "POST",
                &format!("/api/commands/{command}"),
                Some(&t.admin),
                Some(input.clone()),
                &[("idempotency-key", &key)],
            )
            .await;
        assert!(
            status.is_client_error(),
            "{command} {input}: {status} {body}"
        );
        assert!(body["error"]["message"].is_string(), "{body}");
        // The answer goes to whoever sent the secret. A refusal of the
        // input never repeats it; what a handler says about a field it
        // could read is up to the handler.
        let answered = body.to_string();
        if body["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid input")
        {
            assert!(!answered.contains(SECRET), "{command} {input}: {answered}");
            assert!(!answered.contains(&number), "{command} {input}: {answered}");
        }

        // The same through MCP.
        let tool = command.replace('.', "_");
        let result = t.tool(&t.admin, &tool, input.clone()).await;
        assert_eq!(result["isError"], true, "{command} {input}: {result}");
    }

    let recorded = recorded(&t).await;
    for (command, _) in &cases {
        assert!(recorded.contains(command), "{command} was audited");
    }
    assert!(!recorded.contains(SECRET), "{recorded}");
    assert!(!recorded.contains(&number), "{recorded}");
    assert!(!recorded.contains("s3cr3t"), "{recorded}");

    // What the audit log says instead: the kind of error and a fixed
    // sentence, for every failure of these commands.
    let messages: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT error_code, error_message FROM audit_log
         WHERE error_code IS NOT NULL AND input IS NULL ORDER BY 1",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    for (code, message) in &messages {
        let expected = match code.as_str() {
            "validation" => "the input was refused",
            "conflict" => "the request conflicted with the state of a record",
            "not_found" => "a record was not found",
            "forbidden" => "the request was not allowed",
            other => panic!("unexpected error code {other}"),
        };
        assert_eq!(message, expected);
    }
    assert!(!messages.is_empty());
    t.stop().await;
}

#[tokio::test]
async fn a_refusal_of_the_input_names_the_field_and_not_the_value() {
    let t = TestApp::start().await;
    // A command that does not handle secrets keeps its input in the audit
    // log, but the error still does not repeat what was sent.
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": SECRET }))
        .await;
    assert_eq!(status.as_u16(), 422, "{body}");
    assert_eq!(body["error"]["field"], "id");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(!message.contains(SECRET), "{message}");
    t.stop().await;
}
