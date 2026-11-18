//! sqlx caches each connection's prepared statements by their SQL text. Two
//! queries with the same text must therefore bind the same number of values,
//! or the second one to run on a connection fails with "bind message
//! supplies N parameters, but prepared statement requires M".

mod common;

use axum::http::StatusCode;
use common::{PASSWORD, TestApp};
use serde_json::json;

/// A user allowed to read companies both for one company and for all of
/// them lists companies without any condition, as the administrator does:
/// the list must not bind the company the narrower grant named.
#[tokio::test]
async fn a_list_binds_only_the_values_its_sql_uses() {
    let app = TestApp::start().await;
    let company = app.company("OHIO", "USD").await;

    let (_, user_id) = app
        .user_with_rules(
            "both@example.com",
            json!(["company:read"]),
            Some(&company.id),
        )
        .await;
    let everywhere = app
        .create(
            "role",
            json!({ "name": "companies everywhere", "permissions": ["company:read"] }),
        )
        .await;
    let (status, body) = app
        .command(
            &app.admin,
            "user.assign_role",
            json!({ "user_id": user_id, "role_id": everywhere, "company_id": null }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let user = erpcraft_auth::login(&app.kernel, "both@example.com", PASSWORD)
        .await
        .unwrap()
        .token;

    // Requests run one at a time over the same few pooled connections. Each
    // statement text is prepared once per connection and then reused,
    // whoever runs it. Three calls a round, so that no caller keeps to a
    // connection of its own as strict alternation would let it.
    for round in 0..40 {
        for (who, token) in [
            ("user", &user),
            ("admin", &app.admin),
            ("admin", &app.admin),
        ] {
            let (status, body) = app
                .command(token, "entity.list", json!({ "entity": "company" }))
                .await;
            assert_eq!(status, StatusCode::OK, "{who}, round {round}: {body}");
            assert_eq!(body["total"], 1, "{who}, round {round}: {body}");
        }
    }

    app.stop().await;
}
