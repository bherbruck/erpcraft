//! Lists: search, sort, paging and totals, and refusing stale edits.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::{Value, json};

fn codes(page: &Value) -> Vec<&str> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["code"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_list_can_be_searched_sorted_and_paged() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    for (code, name) in [
        ("1100", "Petty cash"),
        ("1200", "Bank"),
        ("5000", "100% cotton"),
    ] {
        t.create(
            "account",
            json!({ "company_id": c.id, "code": code, "name": name, "account_type": "asset" }),
        )
        .await;
    }
    let base = format!("/api/entities/account?company_id={}", c.id);

    // Six accounts in all: three from the fixture, three from above.
    let (status, page) = t.get(&t.admin, &base).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 6);
    assert_eq!(
        codes(&page),
        ["1", "1000", "1100", "1200", "4000", "5000"],
        "the entity's own order is by code"
    );

    // Search looks in code and name, ignoring case.
    let (_, page) = t.get(&t.admin, &format!("{base}&q=CASH")).await;
    assert_eq!(codes(&page), ["1000", "1100"]);
    assert_eq!(page["total"], 2);
    let (_, page) = t.get(&t.admin, &format!("{base}&q=12")).await;
    assert_eq!(codes(&page), ["1200"]);

    // What the user types is text, not a pattern.
    let (_, page) = t.get(&t.admin, &format!("{base}&q=100%25")).await;
    assert_eq!(codes(&page), ["5000"]);
    let (_, page) = t.get(&t.admin, &format!("{base}&q=_")).await;
    assert_eq!(page["total"], 0);

    // Sorting, and paging through the sorted list. The total is that of the
    // whole list, not of the page.
    let (_, page) = t.get(&t.admin, &format!("{base}&sort=-name&limit=2")).await;
    assert_eq!(codes(&page), ["4000", "1100"]);
    assert_eq!(page["total"], 6);
    let (_, page) = t
        .get(&t.admin, &format!("{base}&sort=-name&limit=2&offset=4"))
        .await;
    assert_eq!(codes(&page), ["1", "5000"]);
    let (_, page) = t
        .get(&t.admin, &format!("{base}&sort=account_type,-code&limit=1"))
        .await;
    assert_eq!(codes(&page), ["5000"]);

    let (status, body) = t.get(&t.admin, &format!("{base}&sort=nope")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // The same list as a command, as an agent calls it.
    let (status, page) = t
        .command(
            &t.admin,
            "entity.list",
            json!({ "entity": "account", "filters": { "company_id": c.id }, "q": "bank", "sort": ["-code"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(codes(&page), ["1200"]);

    // An entity that declares no search fields says so.
    let (status, body) = t.get(&t.admin, "/api/entities/gl_entry?q=x").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn totals_and_search_respect_permission_rules() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // May read asset accounts only, and never their names.
    let (token, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!([
                { "allow": "account:read", "where": "record.account_type == 'asset'" },
                { "deny": "account:read", "fields": ["name"] },
            ]),
            Some(&c.id),
        )
        .await;

    let (status, page) = t.get(&token, "/api/entities/account").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 2, "the income account is not counted");
    assert!(page["items"][0].get("name").is_none());
    assert!(page["items"][0]["_version"].is_string());

    // "Cash" is a name. The clerk cannot see names, so a search must not
    // find by them; it still finds by code.
    let (_, page) = t.get(&token, "/api/entities/account?q=cash").await;
    assert_eq!(page["total"], 0);
    let (_, page) = t.get(&token, "/api/entities/account?q=1000").await;
    assert_eq!(codes(&page), ["1000"]);

    // Nor may they learn the order of the names.
    let (status, body) = t.get(&token, "/api/entities/account?sort=name").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _) = t.get(&token, "/api/entities/account?sort=-code").await;
    assert_eq!(status, StatusCode::OK);

    t.stop().await;
}

#[tokio::test]
async fn a_stale_edit_of_a_record_is_refused() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let path = format!("/api/entities/account/{}", c.cash);

    let (_, alice) = t.get(&t.admin, &path).await;
    let (_, bob) = t.get(&t.admin, &path).await;
    assert_eq!(alice["_version"], bob["_version"]);

    // Alice saves first.
    let (status, saved) = t
        .patch(
            &t.admin,
            &path,
            json!({ "name": "Cash on hand", "_version": alice["_version"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_ne!(saved["_version"], alice["_version"]);

    // Bob's copy is now out of date.
    let (status, body) = t
        .patch(
            &t.admin,
            &path,
            json!({ "name": "Till", "_version": bob["_version"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (_, now) = t.get(&t.admin, &path).await;
    assert_eq!(now["name"], "Cash on hand");
    assert_eq!(
        now["_version"], saved["_version"],
        "reading changes nothing"
    );

    // With the version he has now read, it goes through.
    let (status, body) = t
        .command(
            &t.admin,
            "entity.update",
            json!({ "entity": "account", "id": c.cash, "data": { "name": "Till" }, "version": now["_version"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A caller that sends no version is not checked.
    let (status, body) = t.patch(&t.admin, &path, json!({ "name": "Cash" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn documents_can_be_searched_sorted_and_counted() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let other = t.company("OTHER", "USD").await;

    let first = t.draft(&t.admin, &c, "10.00").await;
    let second = t.draft(&t.admin, &c, "20.00").await;
    t.draft(&t.admin, &other, "30.00").await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/documents/journal_entry/{}", common::id(&second)),
            json!({ "version": second["version"], "memo": "Rent for March", "doc_date": "2026-02-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, posted) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": common::id(&first) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");

    let base = format!("/api/documents/journal_entry?company_id={}", c.id);
    let (status, page) = t.get(&t.admin, &base).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 2);
    let (_, all) = t.get(&t.admin, "/api/documents/journal_entry").await;
    assert_eq!(all["total"], 3);

    // By memo and by number.
    let (_, page) = t.get(&t.admin, &format!("{base}&q=rent")).await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["id"], second["id"]);
    let (_, page) = t.get(&t.admin, &format!("{base}&q=JE-2026")).await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["id"], first["id"]);

    let (_, page) = t.get(&t.admin, &format!("{base}&sort=doc_date")).await;
    assert_eq!(page["items"][0]["id"], second["id"]);
    let (_, page) = t
        .get(&t.admin, &format!("{base}&sort=-doc_date&limit=1"))
        .await;
    assert_eq!(page["items"][0]["id"], first["id"]);
    assert_eq!(page["total"], 2);

    let (status, body) = t.get(&t.admin, &format!("{base}&sort=memo")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // Someone who may read one company's journals is not told how many the
    // other company has.
    let token = t
        .user_with_role("reader@example.com", &["journal_entry:read"], &c.id)
        .await;
    let (_, page) = t.get(&token, "/api/documents/journal_entry").await;
    assert_eq!(page["total"], 2);

    t.stop().await;
}

#[tokio::test]
async fn a_list_can_be_filtered_by_an_expression() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let list = |token: String, input: Value| {
        let t = &t;
        async move { t.command(&token, "entity.list", input).await }
    };

    // Any condition the rule language can turn into a query.
    let (status, page) = list(
        t.admin.clone(),
        json!({ "entity": "account", "where": "record.account_type == 'asset' && !record.is_group" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(codes(&page), ["1000"]);
    assert_eq!(page["total"], 1);

    let (_, page) = list(
        t.admin.clone(),
        json!({ "entity": "account", "where": "record.code in ['1000', '4000'] || record.is_group", "sort": ["-code"] }),
    )
    .await;
    assert_eq!(codes(&page), ["4000", "1000", "1"]);

    // Over HTTP, combined with the other parameters.
    let (status, page) = t
        .get(
            &t.admin,
            "/api/entities/account?where=record.account_type%20!%3D%20%27income%27&q=cash",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(codes(&page), ["1000"]);

    // Amounts compare as numbers, dates as dates.
    let draft = t.draft(&t.admin, &c, "250.00").await;
    let (status, posted) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": common::id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{posted}");
    let (status, page) = list(
        t.admin.clone(),
        json!({ "entity": "gl_entry", "where": "record.debit > 100" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["account_id"], c.cash);
    let (_, page) = list(
        t.admin.clone(),
        json!({ "entity": "gl_entry", "where": "record.debit > 1000" }),
    )
    .await;
    assert_eq!(page["total"], 0);

    let documents = |condition: &'static str| {
        let t = &t;
        async move {
            t.command(
                &t.admin,
                "document.list",
                json!({ "doc_type": "journal_entry", "where": condition }),
            )
            .await
        }
    };
    let (status, page) =
        documents("record.doc_date >= '2026-03-01' && record.status == 'posted'").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 1);
    let (_, page) = documents("record.doc_date > '2026-03-15'").await;
    assert_eq!(page["total"], 0);

    // What cannot be a filter is refused with a reason, not ignored.
    for condition in [
        "record.nope == 1",
        "record.code ==",
        "record.code.size() > 2",
    ] {
        let (status, body) = list(
            t.admin.clone(),
            json!({ "entity": "account", "where": condition }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{condition}: {body}"
        );
    }

    // A filter narrows what the caller may see; it never widens it, and it
    // cannot ask about a field that is withheld.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!([
                { "allow": "account:read", "where": "record.account_type == 'asset'" },
                { "deny": "account:read", "fields": ["name"] },
            ]),
            Some(&c.id),
        )
        .await;
    let (status, page) = list(
        clerk.clone(),
        json!({ "entity": "account", "where": "record.account_type == 'income' || record.code == '1000'" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        codes(&page),
        ["1000"],
        "the income account stays out of reach"
    );
    let (status, body) = list(
        clerk,
        json!({ "entity": "account", "where": "record.name == 'Cash'" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_record_can_be_deleted_unless_it_is_in_use() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let spare = t
        .create(
            "account",
            json!({ "company_id": c.id, "code": "9999", "name": "Mistake", "account_type": "asset" }),
        )
        .await;
    let delete = |token: String, entity: &'static str, id: String| {
        let t = &t;
        async move {
            t.send(
                "DELETE",
                &format!("/api/entities/{entity}/{id}"),
                Some(&token),
                None,
                &[],
            )
            .await
        }
    };

    // Deleting needs its own permission, and reaches no further than the
    // caller's rules do.
    let (clerk, _) = t
        .user_with_rules(
            "clerk@example.com",
            json!([
                "account:read",
                "account:update",
                { "allow": "account:delete", "where": "record.account_type == 'expense'" },
            ]),
            Some(&c.id),
        )
        .await;
    let (status, body) = delete(clerk.clone(), "account", spare.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let reader = t
        .user_with_role("reader@example.com", &["account:read"], &c.id)
        .await;
    let (status, _) = delete(reader, "account", spare.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A stale copy cannot delete.
    let (status, body) = t
        .send(
            "DELETE",
            &format!("/api/entities/account/{spare}?version=1"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = delete(t.admin.clone(), "account", spare.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = t
        .get(&t.admin, &format!("/api/entities/account/{spare}"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // It is gone, and its history is still there for an auditor.
    let (status, history) = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "account", "id": spare }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["items"][0]["command"], "entity.delete");
    assert_eq!(
        history["total"], 5,
        "create, three refused attempts, delete"
    );

    // An account with entries, and a company with accounts, stay.
    let draft = t.draft(&t.admin, &c, "10.00").await;
    let (status, _) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": common::id(&draft) }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = delete(t.admin.clone(), "account", c.cash.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["message"],
        "this account is in use and cannot be deleted"
    );
    let (status, _) = delete(t.admin.clone(), "company", c.id.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Ledger entries are never deleted, by anyone.
    let (_, entries) = t.get(&t.admin, "/api/entities/gl_entry").await;
    let entry = common::id(&entries["items"][0]);
    let (status, body) = delete(t.admin.clone(), "gl_entry", entry).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    t.stop().await;
}
