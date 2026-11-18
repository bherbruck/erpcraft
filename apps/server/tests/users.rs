//! Administering users: listing them with their roles, changing them,
//! switching them off, and never losing the last administrator.

mod common;

use axum::http::StatusCode;
use common::{PASSWORD, TestApp};
use serde_json::{Value, json};

async fn run(t: &TestApp, token: &str, command: &str, input: Value) -> Value {
    let (status, body) = t.command(token, command, input).await;
    assert_eq!(status, StatusCode::OK, "{command}: {body}");
    body
}

async fn admin_id(t: &TestApp) -> String {
    let me = run(
        t,
        &t.admin,
        "user.list",
        json!({ "q": common::ADMIN_EMAIL }),
    )
    .await;
    me["items"][0]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn users_are_listed_with_their_roles() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let (_, clerk) = t
        .user_with_rules("clerk@example.com", json!(["account:read"]), Some(&c.id))
        .await;
    let (_, auditor) = t
        .user_with_rules("auditor@example.com", json!(["audit:read"]), None)
        .await;

    let all = run(&t, &t.admin, "user.list", json!({})).await;
    assert_eq!(all["total"], 3);
    let clerk_row = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == clerk.as_str())
        .unwrap();
    assert_eq!(clerk_row["email"], "clerk@example.com");
    assert_eq!(clerk_row["active"], true);
    assert_eq!(
        clerk_row["roles"][0]["role_name"],
        "role for clerk@example.com"
    );
    assert_eq!(clerk_row["roles"][0]["company_id"], c.id.as_str());
    assert_eq!(clerk_row["roles"][0]["company_code"], "ACME");
    // No credentials, and no attributes in a list.
    let text = all.to_string();
    for absent in ["password", "token", "attributes", PASSWORD] {
        assert!(!text.contains(absent), "{absent}");
    }

    // Search, filter, page, and read given users back by id.
    let found = run(&t, &t.admin, "user.list", json!({ "q": "AUDIT" })).await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["id"], auditor.as_str());
    assert_eq!(found["items"][0]["roles"][0]["company_id"], Value::Null);
    let page = run(
        &t,
        &t.admin,
        "user.list",
        json!({ "limit": 1, "offset": 1 }),
    )
    .await;
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let by_id = run(
        &t,
        &t.admin,
        "user.list",
        json!({ "ids": [clerk, auditor], "q": "nobody" }),
    )
    .await;
    assert_eq!(
        by_id["total"], 0,
        "ids narrow the list, they do not widen it"
    );
    let by_id = run(&t, &t.admin, "user.list", json!({ "ids": [clerk] })).await;
    assert_eq!(by_id["total"], 1);

    // One user, with attributes.
    t.set_attributes(&clerk, json!({ "warehouses": ["MAIN"] }))
        .await;
    let one = run(&t, &t.admin, "user.get", json!({ "id": clerk })).await;
    assert_eq!(one["attributes"], json!({ "warehouses": ["MAIN"] }));
    // The generic entity reads and links work too.
    let (status, entity) = t
        .get(&t.admin, &format!("/api/entities/user/{clerk}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{entity}");
    assert_eq!(entity["email"], "clerk@example.com");
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/user",
            json!({ "email": "x@example.com", "name": "X" }),
        )
        .await;
    assert_ne!(
        status,
        StatusCode::CREATED,
        "users are created by user.create"
    );

    // Granting twice is granting once; revoking takes it away.
    let role = t
        .create(
            "role",
            json!({ "name": "Viewer", "permissions": ["item:read"] }),
        )
        .await;
    for _ in 0..2 {
        run(
            &t,
            &t.admin,
            "user.assign_role",
            json!({ "user_id": clerk, "role_id": role, "company_id": c.id }),
        )
        .await;
    }
    let one = run(&t, &t.admin, "user.get", json!({ "id": clerk })).await;
    assert_eq!(one["roles"].as_array().unwrap().len(), 2);
    let revoked = run(
        &t,
        &t.admin,
        "user.revoke_role",
        json!({ "user_id": clerk, "role_id": role, "company_id": c.id }),
    )
    .await;
    assert_eq!(revoked["revoked"], true);
    let again = run(
        &t,
        &t.admin,
        "user.revoke_role",
        json!({ "user_id": clerk, "role_id": role, "company_id": c.id }),
    )
    .await;
    assert_eq!(again["revoked"], false);
    let one = run(&t, &t.admin, "user.get", json!({ "id": clerk })).await;
    assert_eq!(one["roles"].as_array().unwrap().len(), 1);

    // Reading users takes the right, for all companies.
    let (reader_one, _) = t
        .user_with_rules("reader1@example.com", json!(["user:read"]), Some(&c.id))
        .await;
    let (status, _) = t.command(&reader_one, "user.list", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (reader, _) = t
        .user_with_rules("reader@example.com", json!(["user:read"]), None)
        .await;
    run(&t, &reader, "user.list", json!({})).await;
    let (status, _) = t
        .command(
            &reader,
            "user.update",
            json!({ "id": clerk, "name": "Changed" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Every change is in the user's history.
    run(
        &t,
        &t.admin,
        "user.update",
        json!({ "id": clerk, "name": "Clara Clerk" }),
    )
    .await;
    let history = run(
        &t,
        &t.admin,
        "audit.history",
        json!({ "resource": "user", "id": clerk }),
    )
    .await;
    let commands: Vec<&str> = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["command"].as_str().unwrap())
        .collect();
    for command in [
        "user.update",
        "user.revoke_role",
        "user.assign_role",
        "user.set_attributes",
    ] {
        assert!(commands.contains(&command), "{command}: {commands:?}");
    }
    t.stop().await;
}

#[tokio::test]
async fn a_deactivated_user_is_refused_at_once() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let (token, clerk) = t
        .user_with_rules("clerk@example.com", json!(["account:read"]), Some(&c.id))
        .await;
    let (status, _) = t.get(&token, "/api/auth/me").await;
    assert_eq!(status, StatusCode::OK);

    let off = run(&t, &t.admin, "user.deactivate", json!({ "id": clerk })).await;
    assert_eq!(off["active"], false);
    // The token issued before is refused on the very next request.
    let (status, _) = t.get(&token, "/api/auth/me").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = t
        .send(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "email": "clerk@example.com", "password": PASSWORD })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let inactive = run(&t, &t.admin, "user.list", json!({ "active": false })).await;
    assert_eq!(inactive["total"], 1);

    let on = run(&t, &t.admin, "user.activate", json!({ "id": clerk })).await;
    assert_eq!(on["active"], true);
    let (status, _) = t.get(&token, "/api/auth/me").await;
    assert_eq!(status, StatusCode::OK);

    // Switching off is destructive and says so.
    let (_, meta) = t.get(&t.admin, "/api/meta").await;
    let command = |name: &str| {
        meta["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(command("user.deactivate")["destructive"], true);
    assert_eq!(command("user.revoke_role")["destructive"], true);
    assert_eq!(command("user.update")["resource_kind"], "entity");
    assert_eq!(command("user.update")["target"], true);
    assert_eq!(
        command("user.assign_role")["input_schema"]["properties"]["user_id"]["x-field-type"],
        json!({ "kind": "link", "entity": "user", "key": "id" })
    );
    t.stop().await;
}

#[tokio::test]
async fn the_last_administrator_stays_one() {
    let t = TestApp::start().await;
    let admin = admin_id(&t).await;

    // Alone, the administrator can neither stop being one nor be switched off.
    for (command, input) in [
        ("user.update", json!({ "id": admin, "is_admin": false })),
        ("user.deactivate", json!({ "id": admin })),
    ] {
        let (status, body) = t.command(&t.admin, command, input).await;
        assert_eq!(status, StatusCode::CONFLICT, "{command}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("last active administrator"),
            "{body}"
        );
    }

    // Only an administrator grants administrator rights.
    let (manager, manager_id) = t
        .user_with_rules(
            "manager@example.com",
            json!(["user:update", "user:read"]),
            None,
        )
        .await;
    let (status, body) = t
        .command(
            &manager,
            "user.update",
            json!({ "id": manager_id, "is_admin": true }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Nor does a manager switch an administrator off.
    let (status, body) = t
        .command(&manager, "user.deactivate", json!({ "id": admin }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // With a second administrator, the first may step down.
    let second = run(
        &t,
        &t.admin,
        "user.update",
        json!({ "id": manager_id, "is_admin": true }),
    )
    .await;
    assert_eq!(second["is_admin"], true);
    let stepped = run(
        &t,
        &t.admin,
        "user.update",
        json!({ "id": admin, "is_admin": false }),
    )
    .await;
    assert_eq!(stepped["is_admin"], false);
    // The new last administrator keeps the rights.
    let (status, _) = t
        .command(
            &manager,
            "user.update",
            json!({ "id": manager_id, "is_admin": false }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // An inactive administrator does not count.
    run(
        &t,
        &manager,
        "user.update",
        json!({ "id": admin, "is_admin": true }),
    )
    .await;
    run(&t, &manager, "user.deactivate", json!({ "id": admin })).await;
    let (status, _) = t
        .command(&manager, "user.deactivate", json!({ "id": manager_id }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    t.stop().await;
}

#[tokio::test]
async fn nobody_hands_on_more_than_they_hold() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let role = |name: &str, permissions: Value| {
        let t = &t;
        let name = name.to_owned();
        async move {
            t.create("role", json!({ "name": name, "permissions": permissions }))
                .await
        }
    };
    let everything = role("Everything", json!(["*"])).await;
    let items = role("Items", json!(["item:read"])).await;
    let all_items = role("All of items", json!(["item:*"])).await;
    let (manager, manager_id) = t
        .user_with_rules(
            "manager@example.com",
            json!([
                "user:assign_role",
                "user:update",
                "user:read",
                "role:create",
                "role:update",
                "role:read",
                "item:read"
            ]),
            None,
        )
        .await;
    let (_, clerk) = t
        .user_with_rules("clerk@example.com", json!(["account:read"]), Some(&c.id))
        .await;
    let assign = |user: &str, role: &str| json!({ "user_id": user, "role_id": role });

    // What the manager holds, they may grant to someone else.
    let (status, body) = t
        .command(&manager, "user.assign_role", assign(&clerk, &items))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Not more, and nothing to themselves.
    for (user, role) in [
        (&clerk, &everything),
        (&clerk, &all_items),
        (&manager_id, &items),
        (&manager_id, &everything),
    ] {
        let (status, body) = t
            .command(&manager, "user.assign_role", assign(user, role))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {body}");
    }
    // Nor take away what they could not give.
    run(
        &t,
        &t.admin,
        "user.assign_role",
        assign(&clerk, &everything),
    )
    .await;
    let (status, body) = t
        .command(&manager, "user.revoke_role", assign(&clerk, &everything))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = t
        .command(&manager, "user.revoke_role", assign(&clerk, &items))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Roles are written by those who hold what they give.
    let (status, body) = t
        .post(
            &manager,
            "/api/entities/role",
            json!({ "name": "Mine", "permissions": ["*"] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, mine) = t
        .post(
            &manager,
            "/api/entities/role",
            json!({ "name": "Mine", "permissions": ["item:read"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{mine}");
    let (status, body) = t
        .patch(
            &manager,
            &format!("/api/entities/role/{}", mine["id"].as_str().unwrap()),
            json!({ "permissions": ["item:read", "vendor:read"] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["field"], "permissions");
    let (status, body) = t
        .patch(
            &manager,
            &format!("/api/entities/role/{everything}"),
            json!({ "name": "Renamed" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // Attributes widen what rules reach: not one's own, not an
    // administrator's, and not a user with a role one could not grant.
    let (status, body) = t
        .command(
            &manager,
            "user.set_attributes",
            json!({ "user_id": manager_id, "attributes": { "warehouses": ["ALL"] } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let admin = admin_id(&t).await;
    let (status, _) = t
        .command(
            &manager,
            "user.set_attributes",
            json!({ "user_id": admin, "attributes": {} }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = t
        .command(
            &manager,
            "user.set_attributes",
            json!({ "user_id": clerk, "attributes": {} }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the clerk holds Everything: {body}"
    );
    run(
        &t,
        &t.admin,
        "user.revoke_role",
        assign(&clerk, &everything),
    )
    .await;
    // The clerk's company role gives account:read, which the manager does
    // not hold: still refused.
    let (status, _) = t
        .command(
            &manager,
            "user.set_attributes",
            json!({ "user_id": clerk, "attributes": {} }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, plain) = t
        .user_with_rules("plain@example.com", json!(["item:read"]), None)
        .await;
    let (status, body) = t
        .command(
            &manager,
            "user.set_attributes",
            json!({ "user_id": plain, "attributes": { "warehouses": ["MAIN"] } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn the_directory_and_roles_are_read_for_all_companies_only() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let reader = t
        .user_with_role("reader@example.com", &["*:read"], &c.id)
        .await;
    for entity in ["user", "role"] {
        let (status, body) = t.get(&reader, &format!("/api/entities/{entity}")).await;
        let none = status != StatusCode::OK || body["total"] == 0;
        assert!(none, "{entity}: {status} {body}");
        let (status, body) = t
            .command(&reader, "entity.list", json!({ "entity": entity }))
            .await;
        assert!(
            status != StatusCode::OK || body["total"] == 0,
            "{entity}: {body}"
        );
    }
    let admin = admin_id(&t).await;
    let (status, _) = t.get(&reader, &format!("/api/entities/user/{admin}")).await;
    assert_ne!(status, StatusCode::OK);
    // Shared records that every company uses stay readable under one
    // company's grant.
    t.create("item", json!({ "code": "W", "name": "Widget" }))
        .await;
    let (status, body) = t.get(&reader, "/api/entities/item").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);
    // With the grant for all companies, the directory is readable.
    let (global, _) = t
        .user_with_rules(
            "global@example.com",
            json!(["user:read", "role:read"]),
            None,
        )
        .await;
    let (status, body) = t.get(&global, "/api/entities/user").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["total"].as_i64().unwrap() >= 3);
    t.stop().await;
}
