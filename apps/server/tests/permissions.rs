//! Fine-grained permissions: field rules, row rules, explicit denies.

mod common;

use axum::http::StatusCode;
use common::{TestApp, dec, id};
use serde_json::{Value, json};

fn codes(list: &Value) -> Vec<&str> {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn field_rules_hide_fields_and_limit_what_can_be_changed() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let rules = json!([
        "account:read",
        { "deny": "account:read", "fields": ["name"] },
        { "allow": "account:update", "fields": ["active"] },
    ]);
    let (clerk, _) = t
        .user_with_rules("clerk@example.com", rules, Some(&c.id))
        .await;
    let cash = format!("/api/entities/account/{}", c.cash);

    // The withheld field is absent from lists and single reads; the rest is there.
    let (status, list) = t.get(&clerk, "/api/entities/account").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert!(
        items
            .iter()
            .all(|a| a.get("name").is_none() && a["code"].is_string() && a["id"].is_string())
    );
    let (status, one) = t.get(&clerk, &cash).await;
    assert_eq!(status, StatusCode::OK);
    assert!(one.get("name").is_none());
    assert_eq!(one["code"], "1000");
    // The administrator still sees it.
    let (_, full) = t.get(&t.admin, &cash).await;
    assert_eq!(full["name"], "Cash");

    // A hidden field cannot be probed through a filter.
    let (status, _) = t.get(&clerk, "/api/entities/account?name=Cash").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, found) = t.get(&clerk, "/api/entities/account?code=1000").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(codes(&found), ["1000"]);

    // Only the granted field can be changed, and the reply hides the same field.
    let (status, changed) = t.patch(&clerk, &cash, json!({ "active": false })).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["active"], false);
    assert!(changed.get("name").is_none());
    for attempt in [
        json!({ "name": "Petty cash" }),
        json!({ "code": "1001" }),
        json!({ "active": true, "name": "X" }),
    ] {
        let (status, body) = t.patch(&clerk, &cash, attempt).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    let (_, after) = t.get(&t.admin, &cash).await;
    assert_eq!(
        (
            after["name"].as_str(),
            after["code"].as_str(),
            after["active"].as_bool()
        ),
        (Some("Cash"), Some("1000"), Some(false))
    );

    t.stop().await;
}

#[tokio::test]
async fn row_rules_filter_lists_and_guard_every_write() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let mine = "record.account_type in user.account_types";
    let rules = json!([
        { "allow": "account:read", "where": mine },
        { "allow": "account:update", "where": mine },
        { "allow": "account:create", "where": mine },
        { "deny": "account:read", "where": "record.is_group" },
    ]);
    let (clerk, clerk_id) = t
        .user_with_rules("clerk@example.com", rules, Some(&c.id))
        .await;

    // With no attribute set, the rule matches nothing.
    let (status, list) = t.get(&clerk, "/api/entities/account").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(codes(&list).is_empty());

    t.set_attributes(&clerk_id, json!({ "account_types": ["asset"] }))
        .await;

    // Asset accounts only, and the deny removes the group account.
    let (_, list) = t.get(&clerk, "/api/entities/account").await;
    assert_eq!(codes(&list), ["1000"]);
    let (status, _) = t
        .get(&clerk, &format!("/api/entities/account/{}", c.revenue))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = t
        .get(&clerk, &format!("/api/entities/account/{}", c.group))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Paging counts only what the clerk may see.
    let (_, page) = t
        .get(&clerk, "/api/entities/account?limit=1&offset=1")
        .await;
    assert!(codes(&page).is_empty());

    // Updates: reachable records only, and a record cannot be pushed out of reach.
    let cash = format!("/api/entities/account/{}", c.cash);
    let (status, body) = t
        .patch(&clerk, &cash, json!({ "name": "Cash on hand" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .patch(&clerk, &cash, json!({ "account_type": "income" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (_, after) = t.get(&t.admin, &cash).await;
    assert_eq!(
        (after["name"].as_str(), after["account_type"].as_str()),
        (Some("Cash on hand"), Some("asset"))
    );
    let (status, _) = t
        .patch(
            &clerk,
            &format!("/api/entities/account/{}", c.revenue),
            json!({ "name": "Sales" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Creates are checked against the new record.
    let new = |code: &str, kind: &str| json!({ "company_id": c.id, "code": code, "name": code, "account_type": kind });
    let (status, body) = t
        .post(&clerk, "/api/entities/account", new("1100", "asset"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = t
        .post(&clerk, "/api/entities/account", new("4100", "income"))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Attributes take effect on the next request.
    t.set_attributes(&clerk_id, json!({ "account_types": ["asset", "income"] }))
        .await;
    let (_, list) = t.get(&clerk, "/api/entities/account").await;
    assert_eq!(codes(&list), ["1000", "1100", "4000"]);

    // A rule on one company says nothing about another.
    let other = t.company("OTHER", "USD").await;
    let (status, _) = t
        .get(&clerk, &format!("/api/entities/account/{}", other.cash))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    t.stop().await;
}

#[tokio::test]
async fn a_deny_rule_separates_duties_on_documents() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    // Anyone with this role can work with journals, but nobody posts their own.
    let rules = json!([
        "journal_entry:*",
        "account:read",
        { "deny": "journal_entry:post", "where": "record.created_by == user.id" },
    ]);
    let (alice, _) = t
        .user_with_rules("alice@example.com", rules.clone(), Some(&c.id))
        .await;
    let (bob, _) = t
        .user_with_rules("bob@example.com", rules, Some(&c.id))
        .await;

    let draft = id(&t.draft(&alice, &c, "120.00").await);
    let (status, body) = t
        .command(&alice, "journal_entry.post", json!({ "id": draft }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let result = t
        .tool(&alice, "journal_entry_post", json!({ "id": draft }))
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 0);

    let (status, posted) = t
        .command(&bob, "journal_entry.post", json!({ "id": draft }))
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    assert_eq!(posted["status"], "posted");

    // The tool is still offered to alice: she can post what others drafted.
    let tools = t.mcp(&alice, "tools/list", json!({})).await;
    assert!(
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "journal_entry_post")
    );

    t.stop().await;
}

#[tokio::test]
async fn row_rules_apply_to_document_lists_and_reads() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let own = "record.created_by == user.id";
    let rules = json!([
        "journal_entry:create",
        "account:read",
        { "allow": "journal_entry:read", "where": own },
        { "allow": "journal_entry:update", "where": format!("{own} && record.status == 'draft'") },
    ]);
    let (alice, _) = t
        .user_with_rules("alice@example.com", rules.clone(), Some(&c.id))
        .await;
    let (bob, _) = t
        .user_with_rules("bob@example.com", rules, Some(&c.id))
        .await;

    let hers = id(&t.draft(&alice, &c, "10.00").await);
    let his = id(&t.draft(&bob, &c, "20.00").await);

    let (status, list) = t.get(&alice, "/api/documents/journal_entry").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let ids: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [hers.as_str()]);
    let (_, all) = t.get(&t.admin, "/api/documents/journal_entry").await;
    assert_eq!(all["items"].as_array().unwrap().len(), 2);

    let (status, _) = t
        .get(&alice, &format!("/api/documents/journal_entry/{his}"))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .patch(
            &alice,
            &format!("/api/documents/journal_entry/{his}"),
            json!({ "version": 1, "memo": "mine now" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = t
        .patch(
            &alice,
            &format!("/api/documents/journal_entry/{hers}"),
            json!({ "version": 1, "memo": "edited" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn conditions_on_amounts_filter_and_totals_fail_closed() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    for amount in ["40.00", "250.50"] {
        let doc = id(&t.draft(&t.admin, &c, amount).await);
        let (status, _) = t
            .command(&t.admin, "journal_entry.post", json!({ "id": doc }))
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let rules =
        json!([{ "allow": "gl_entry:read", "where": "record.debit > 100 || record.credit > 100" }]);
    let (auditor, _) = t
        .user_with_rules("auditor@example.com", rules, Some(&c.id))
        .await;

    let (status, list) = t.get(&auditor, "/api/entities/gl_entry").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|e| dec(&e["debit"]) == 250.5 || dec(&e["credit"]) == 250.5)
    );

    // A total over all entries would reveal the rows the rule hides.
    let (status, body) = t
        .command(&auditor, "gl.trial_balance", json!({ "company_id": c.id }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_rule_is_checked_when_its_role_is_saved() {
    let t = TestApp::start().await;
    let save = |rules: Value| {
        let t = &t;
        async move {
            t.post(
                &t.admin,
                "/api/entities/role",
                json!({ "name": "r", "permissions": rules }),
            )
            .await
        }
    };

    for (rules, expect) in [
        (
            json!([{ "allow": "account:read", "where": "record.nope == 1" }]),
            "no field `nope`",
        ),
        (
            json!([{ "allow": "account:read", "where": "record.code ==" }]),
            "rule `record.code ==`",
        ),
        (
            json!([{ "allow": "account:read", "where": "record.code.startsWith('1')" }]),
            "cannot be used in a rule",
        ),
        (
            json!([{ "allow": "account:read", "where": "record.code" }]),
            "not true or false",
        ),
        (
            json!([{ "allow": "*:read", "where": "record.code == '1'" }]),
            "must name one resource",
        ),
        (
            json!([{ "allow": "user:create", "where": "record.email == 'x'" }]),
            "not an entity or a document type",
        ),
        (
            json!([{ "deny": "account:read", "fields": ["salary"] }]),
            "has no field 'salary'",
        ),
        (
            json!([{ "deny": "journal_entry:read", "fields": ["memo"] }]),
            "field rules apply to entities",
        ),
        (
            json!([{ "allow": "account:read", "when": "true" }]),
            "unknown key 'when'",
        ),
        (json!(["account"]), "resource:action"),
        (json!("account:read"), "list"),
    ] {
        let (status, body) = save(rules.clone()).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{rules} -> {body}"
        );
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(expect),
            "{rules}: expected `{expect}` in `{message}`"
        );
    }
    assert_eq!(t.count("SELECT count(*) FROM role").await, 0);

    // A valid role is saved, can be changed only to another valid one, and
    // shows up on the actor.
    let rules = json!([
        "account:read",
        { "deny": "account:read", "fields": ["name"] },
        { "allow": "journal_entry:read", "where": "record.created_by == user.id && record.doc_date >= '2026-01-01'" },
    ]);
    let (status, role) = save(rules).await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let path = format!("/api/entities/role/{}", id(&role));
    let (status, _) = t
        .patch(
            &t.admin,
            &path,
            json!({ "permissions": [{ "allow": "account:read", "where": "oops" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let c = t.company("ACME", "USD").await;
    let (user, _) = t
        .user_with_rules(
            "u@example.com",
            json!([{ "allow": "account:read", "where": "record.active" }]),
            Some(&c.id),
        )
        .await;
    let (_, me) = t.get(&user, "/api/auth/me").await;
    assert_eq!(
        me["grants"][0]["rules"][0],
        json!({ "allow": "account:read", "where": "record.active" })
    );

    t.stop().await;
}
