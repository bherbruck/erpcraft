//! What the API tells an interface, so that it need not guess: errors that
//! name the field, the line and the reason; commands that describe
//! themselves; schemas of what commands return; and the ledger entries a
//! posting writes, in the document and in a preview.

mod common;

use axum::http::StatusCode;
use common::{Company, TestApp, dec, id};
use serde_json::{Value, json};

fn error(body: &Value) -> &Value {
    &body["error"]
}

/// Draft a journal entry with these lines.
async fn journal(t: &TestApp, token: &str, company: &str, lines: Value) -> (StatusCode, Value) {
    t.post(
        token,
        "/api/documents/journal_entry",
        json!({ "company_id": company, "doc_date": "2026-03-15", "body": { "lines": lines } }),
    )
    .await
}

async fn run(t: &TestApp, name: &str, input: Value) -> Value {
    let (status, body) = t.command(&t.admin, name, input).await;
    assert_eq!(status, StatusCode::OK, "{name}: {body}");
    body
}

// ------------------------------------------------------------------- errors

#[tokio::test]
async fn errors_name_the_field() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let account = |data: Value| t.post(&t.admin, "/api/entities/account", data);

    // A value outside the options of a field.
    let (status, body) =
        account(json!({ "company_id": c.id, "code": "9", "name": "X", "account_type": "bogus" }))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        *error(&body),
        json!({
            "code": "validation",
            "message": "field 'account_type' must be one of: asset, liability, equity, income, expense",
            "field": "account_type",
        })
    );

    // A required field left out, an unknown one, one set by the system.
    let (_, body) =
        account(json!({ "company_id": c.id, "code": "9", "account_type": "asset" })).await;
    assert_eq!(error(&body)["message"], "field 'name' is required");
    assert_eq!(error(&body)["field"], "name");
    let (_, body) = account(
        json!({ "company_id": c.id, "code": "9", "name": "X", "account_type": "asset", "colour": "red" }),
    )
    .await;
    assert_eq!(error(&body)["message"], "unknown field 'colour'");
    assert_eq!(error(&body)["field"], "colour");
    let (_, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/fiscal_period/{}", c.period),
            json!({ "status": "closed" }),
        )
        .await;
    assert_eq!(error(&body)["field"], "status");

    // More decimal places than the column keeps.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/exchange_rate",
            json!({ "from_currency": "EUR", "to_currency": "USD", "rate_date": "2026-03-01", "rate": "1.1234567890123456789" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        error(&body)["message"],
        "field 'rate' has at most 18 decimal places"
    );
    assert_eq!(error(&body)["field"], "rate");

    // A constraint of the table: the field it is about.
    let (status, body) =
        account(json!({ "company_id": c.id, "code": "", "name": "X", "account_type": "asset" }))
            .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "code");

    // The input of a command that cannot be read: which field, in words.
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": "not-an-id" }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "id");
    let message = error(&body)["message"].as_str().unwrap();
    assert!(
        message
            .starts_with("invalid input: field 'id' is not valid: it must be the id of a record"),
        "{message}"
    );
    assert!(!message.contains("UUID parsing failed"), "{message}");
    let (_, body) = t.command(&t.admin, "journal_entry.post", json!({})).await;
    assert_eq!(
        error(&body)["message"],
        "invalid input: field 'id' is required"
    );
    assert_eq!(error(&body)["field"], "id");
    let (_, body) = t
        .command(
            &t.admin,
            "journal_entry.post",
            json!({ "id": c.id, "force": true }),
        )
        .await;
    assert_eq!(error(&body)["field"], "force");
    // The same through a REST-shaped route.
    let (status, body) = t.get(&t.admin, "/api/entities/account/42").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "id");
    let (_, body) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({ "company_id": c.id, "doc_date": "15 March", "body": { "lines": [] } }),
        )
        .await;
    assert_eq!(error(&body)["field"], "doc_date");
    assert!(
        error(&body)["message"]
            .as_str()
            .unwrap()
            .contains("written as 2026-03-15"),
        "{body}"
    );

    t.stop().await;
}

#[tokio::test]
async fn errors_name_the_line() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // A rule of the document type, on the second line.
    let (status, body) = journal(
        &t,
        &t.admin,
        &c.id,
        json!([
            { "account_id": c.cash, "debit": "10.00" },
            { "account_id": c.revenue, "credit": "-10.00" },
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        *error(&body),
        json!({
            "code": "validation",
            "message": "line 2: amounts cannot be negative",
            "collection": "lines",
            "line": 2,
            "field": "credit",
        })
    );

    // About the line as a whole: no field.
    let (_, body) = journal(
        &t,
        &t.admin,
        &c.id,
        json!([{ "account_id": c.cash, "debit": "10.00", "credit": "10.00" }]),
    )
    .await;
    assert_eq!(error(&body)["line"], 1);
    assert_eq!(error(&body).get("field"), None);

    // A line that cannot be read at all.
    let (status, body) = journal(
        &t,
        &t.admin,
        &c.id,
        json!([
            { "account_id": c.cash, "debit": "10.00" },
            { "account_id": c.revenue, "credit": "10.00" },
            { "account_id": "cash", "debit": "1.00" },
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["collection"], "lines");
    assert_eq!(error(&body)["line"], 3);
    assert_eq!(error(&body)["field"], "account_id");
    assert!(
        error(&body)["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid journal entry body: line 3: field 'account_id' is not valid: "),
        "{body}"
    );
    let (_, body) = journal(&t, &t.admin, &c.id, json!([{ "debit": "10.00" }])).await;
    assert_eq!(
        *error(&body),
        json!({
            "code": "validation",
            "message": "invalid journal entry body: line 1: field 'account_id' is required",
            "collection": "lines",
            "line": 1,
            "field": "account_id",
        })
    );
    // A field of the body itself.
    let (_, body) = t
        .post(
            &t.admin,
            "/api/documents/journal_entry",
            json!({ "company_id": c.id, "doc_date": "2026-03-15", "body": { "lines": [], "notes": 1 } }),
        )
        .await;
    assert_eq!(error(&body)["field"], "notes");
    assert_eq!(error(&body).get("line"), None);

    t.stop().await;
}

#[tokio::test]
async fn conflicts_say_why() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let reason = |body: &Value| body["error"]["reason"].clone();

    // duplicate: a unique value is taken.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/account",
            json!({ "company_id": c.id, "code": "1000", "name": "Cash again", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        *error(&body),
        json!({
            "code": "conflict",
            "message": "an account with this Company and Code already exists",
            "reason": "duplicate",
            "field": "code",
        })
    );

    // stale: the record or document changed after it was read.
    let path = format!("/api/entities/account/{}", c.cash);
    let (status, body) = t
        .patch(&t.admin, &path, json!({ "name": "Till", "_version": "1" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(reason(&body), "stale");
    let (status, body) = t
        .send(
            "DELETE",
            &format!("{path}?version=1"),
            Some(&t.admin),
            None,
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(reason(&body), "stale");
    let draft = t.draft(&t.admin, &c, "150.00").await;
    let doc_path = format!("/api/documents/journal_entry/{}", id(&draft));
    let (status, body) = t
        .patch(&t.admin, &doc_path, json!({ "version": 7, "memo": "late" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(reason(&body), "stale");

    // state: the document is not in a state that allows this.
    run(&t, "journal_entry.post", json!({ "id": draft["id"] })).await;
    let (status, body) = t
        .patch(&t.admin, &doc_path, json!({ "version": 2, "memo": "late" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(reason(&body), "state");
    let (status, body) = t.send("DELETE", &doc_path, Some(&t.admin), None, &[]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(reason(&body), "state");
    run(&t, "journal_entry.reverse", json!({ "id": draft["id"] })).await;
    let (status, body) = t
        .command(&t.admin, "journal_entry.post", json!({ "id": draft["id"] }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        *error(&body),
        json!({
            "code": "conflict",
            "message": "a reversed journal entry cannot be posted",
            "reason": "state",
        })
    );

    // in_use: something still refers to the record.
    let (status, body) = t.send("DELETE", &path, Some(&t.admin), None, &[]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(reason(&body), "in_use");

    // A conflict that is none of these has no reason.
    let key = [("idempotency-key", "one")];
    let other = t.draft(&t.admin, &c, "20.00").await;
    let post = |doc: &Value| {
        t.send(
            "POST",
            "/api/commands/journal_entry.post",
            Some(&t.admin),
            Some(json!({ "id": doc["id"] })),
            &key,
        )
    };
    let (status, _) = post(&other).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post(&draft).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(error(&body)["code"], "conflict");
    assert_eq!(error(&body).get("reason"), None, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn tool_errors_carry_the_same_structure() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    let result = t
        .tool(
            &t.admin,
            "document_create",
            json!({
                "doc_type": "journal_entry",
                "company_id": c.id,
                "doc_date": "2026-03-15",
                "body": { "lines": [
                    { "account_id": c.cash, "debit": "10.00" },
                    { "account_id": c.revenue, "credit": "-10.00" },
                ]},
            }),
        )
        .await;
    assert_eq!(result["isError"], true);
    let expected = json!({ "error": {
        "code": "validation",
        "message": "line 2: amounts cannot be negative",
        "collection": "lines",
        "line": 2,
        "field": "credit",
    }});
    assert_eq!(result["structuredContent"], expected);
    // The text is the same object, for a client that reads only text.
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, expected);

    let draft = t.draft(&t.admin, &c, "150.00").await;
    t.tool(&t.admin, "journal_entry_post", json!({ "id": draft["id"] }))
        .await;
    let result = t
        .tool(
            &t.admin,
            "document_update",
            json!({ "doc_type": "journal_entry", "id": draft["id"], "version": 2, "memo": "late" }),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"]["code"], "conflict");
    assert_eq!(result["structuredContent"]["error"]["reason"], "state");

    let result = t
        .tool(&t.admin, "journal_entry_post", json!({ "id": "nope" }))
        .await;
    assert_eq!(result["structuredContent"]["error"]["field"], "id");

    t.stop().await;
}

// ----------------------------------------------------------------- commands

async fn describe(t: &TestApp) -> Value {
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    meta
}

fn command<'a>(meta: &'a Value, name: &str) -> &'a Value {
    meta["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no command {name}"))
}

#[tokio::test]
async fn commands_describe_themselves() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;

    // An action on one document, from one state.
    let post = command(&meta, "journal_entry.post");
    assert_eq!(post["label"], "Post");
    assert_eq!(post["resource"], "journal_entry");
    assert_eq!(post["resource_kind"], "document");
    assert_eq!(post["action"], "post");
    assert_eq!(post["target"], true);
    assert_eq!(post["from_states"], json!(["draft"]));
    assert_eq!(post["destructive"], false);
    assert_eq!(post["report"], Value::Null);
    assert_eq!(post["read_only"], false);
    assert_eq!(post["primary_when"], Value::Null);

    // Which action comes first on a payment run: proposing while it has
    // no lines, approving once it has some.
    assert_eq!(
        command(&meta, "payment_run.propose")["primary_when"],
        json!({ "states": ["draft"], "empty": "lines", "not_empty": null })
    );
    assert_eq!(
        command(&meta, "payment_run.approve")["primary_when"],
        json!({ "states": ["draft"], "not_empty": "lines", "empty": null })
    );
    assert_eq!(
        command(&meta, "payment_run.cancel")["primary_when"],
        Value::Null
    );

    let reverse = command(&meta, "journal_entry.reverse");
    assert_eq!(reverse["label"], "Reverse");
    assert_eq!(reverse["from_states"], json!(["posted"]));
    assert_eq!(reverse["destructive"], true);

    // Commands declared through one struct for several steps.
    let cancel = command(&meta, "purchase_order.cancel");
    assert_eq!(cancel["from_states"], json!(["draft", "open"]));
    assert_eq!(cancel["destructive"], true);
    let approve = command(&meta, "purchase_order.approve");
    assert_eq!(approve["from_states"], json!(["draft"]));
    assert_eq!(approve["destructive"], false);
    assert_eq!(
        command(&meta, "production_order.complete")["from_states"],
        json!(["released"])
    );
    assert_eq!(
        command(&meta, "stock_transfer.reverse")["from_states"],
        json!(["posted", "in_transit"])
    );

    // An action on one record of an entity.
    let close = command(&meta, "fiscal_period.close");
    assert_eq!(close["label"], "Close");
    assert_eq!(close["resource"], "fiscal_period");
    assert_eq!(close["resource_kind"], "entity");
    assert_eq!(close["target"], true);
    assert_eq!(close["from_states"], json!(["open"]));

    // A report: about no record, and worth a page.
    let report = command(&meta, "gl.trial_balance");
    assert_eq!(report["label"], "Trial balance");
    assert_eq!(report["resource"], "gl");
    assert_eq!(report["resource_kind"], Value::Null);
    assert_eq!(report["target"], false);
    assert_eq!(report["read_only"], true);
    assert_eq!(report["report"], json!({ "title": "Trial balance" }));
    for name in [
        "gl.balance_sheet",
        "gl.income_statement",
        "gl.account_ledger",
        "stock.on_hand",
        "open_item.list",
        "tax.report",
    ] {
        assert!(
            command(&meta, name)["report"]["title"].is_string(),
            "{name}"
        );
    }

    // The generic commands take the entity or document type as input; they
    // are not about a resource named `entity`.
    let get = command(&meta, "entity.get");
    assert_eq!(get["resource_kind"], Value::Null);
    assert_eq!(get["target"], false);
    assert_eq!(command(&meta, "entity.delete")["destructive"], true);
    assert_eq!(command(&meta, "document.delete")["destructive"], true);

    // What holds for every command.
    let doc_types: Vec<&Value> = meta["doc_types"].as_array().unwrap().iter().collect();
    for c in meta["commands"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        assert!(!c["label"].as_str().unwrap().is_empty(), "{name}");
        assert_eq!(
            format!(
                "{}.{}",
                name.split('.').next().unwrap(),
                c["action"].as_str().unwrap()
            ),
            name
        );
        assert!(c["input_schema"].is_object(), "{name}");
        assert!(
            c["output_schema"].is_object() || c["output_schema"].is_boolean(),
            "{name}"
        );
        if c["report"].is_object() {
            assert_eq!(c["read_only"], true, "{name}");
        }
        if c["target"] == true {
            assert!(
                c["input_schema"]["properties"]["id"].is_object(),
                "{name} acts on a record but takes no id"
            );
            assert!(c["resource_kind"].is_string(), "{name}");
        }
        // The states a command applies from are states of its document
        // type, or values of its entity's state field.
        let states = c["from_states"].as_array().unwrap();
        if !states.is_empty() {
            let known: Vec<Value> = if c["resource_kind"] == "document" {
                let doc_type = doc_types
                    .iter()
                    .find(|d| d["name"] == c["resource"])
                    .unwrap();
                doc_type["states"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s["name"].clone())
                    .collect()
            } else {
                assert_eq!(c["resource_kind"], "entity", "{name}");
                let entity = meta["entities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|e| e["name"] == c["resource"])
                    .unwrap();
                let field = entity["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|f| f["name"] == entity["state_field"])
                    .unwrap_or_else(|| panic!("{name}: the entity has no state field"));
                field["type"]["values"].as_array().unwrap().clone()
            };
            for state in states {
                assert!(known.contains(state), "{name}: {state}");
            }
        }
        // Every action on a document says which states it applies from.
        if c["resource_kind"] == "document" && c["target"] == true && c["read_only"] == false {
            assert!(!states.is_empty(), "{name} names no state");
        }
    }

    // A field that holds the id of something says what, so that no
    // interface has to show a bare id: in every document body, a field
    // named `*_id` is a reference of some kind, never a plain `uuid`.
    let plain_ids = |owner: &str, fields: &Value| -> Vec<String> {
        fields
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["type"]["kind"] == "uuid")
            .map(|f| f["name"].as_str().unwrap())
            .filter(|name| name.ends_with("_id"))
            .map(|name| format!("{owner}.{name}"))
            .collect()
    };
    let mut bare = vec![];
    for d in &doc_types {
        let name = d["name"].as_str().unwrap();
        bare.extend(plain_ids(name, &d["fields"]));
        for c in d["collections"].as_array().unwrap() {
            let owner = format!("{name}.{}", c["name"].as_str().unwrap());
            bare.extend(plain_ids(&owner, &c["fields"]));
            // Every line can be told from the others and has its place.
            for required in ["id", "line_no"] {
                assert!(
                    c["fields"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|f| f["name"] == required),
                    "{owner} does not declare '{required}'"
                );
            }
        }
    }
    assert!(
        bare.is_empty(),
        "ids that are not declared as references: {bare:#?}"
    );

    // The same through MCP's own description of the system.
    let result = t
        .tool(&t.admin, "meta_describe", json!({ "module": "accounting" }))
        .await;
    let post = command(&result["structuredContent"], "year_end_close.post");
    assert_eq!(post["from_states"], json!(["draft"]));
    assert_eq!(post["target"], true);

    t.stop().await;
}

/// Every input an interface may have to label has a title, and a
/// description that says what to enter.
#[tokio::test]
async fn input_properties_are_titled_and_described() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let post = command(&meta, "journal_entry.post");
    assert_eq!(post["input_schema"]["properties"]["id"]["title"], "ID");
    assert_eq!(
        post["input_schema"]["properties"]["id"]["description"],
        "Journal entry id."
    );
    let sheet = command(&meta, "gl.balance_sheet");
    assert_eq!(
        sheet["input_schema"]["properties"]["company_id"]["title"],
        "Company"
    );
    assert_eq!(
        sheet["input_schema"]["properties"]["as_of"]["title"],
        "As of"
    );

    let mut missing = vec![];
    for c in meta["commands"].as_array().unwrap() {
        let Some(properties) = c["input_schema"]["properties"].as_object() else {
            continue;
        };
        for (property, schema) in properties {
            let name = format!("{}.{property}", c["name"].as_str().unwrap());
            assert!(schema["title"].is_string(), "{name} has no title");
            if !schema["description"].is_string() {
                missing.push(name);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "inputs without a description: {missing:#?}"
    );
    t.stop().await;
}

// ----------------------------------------------------------- output schemas

/// Fail unless `output` is what the command's published schema says.
fn assert_matches(meta: &Value, name: &str, output: &Value) {
    let schema = &command(meta, name)["output_schema"];
    let validator = jsonschema::validator_for(schema)
        .unwrap_or_else(|e| panic!("{name}: the output schema is not a valid schema: {e}"));
    let problems: Vec<String> = validator
        .iter_errors(output)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        problems.is_empty(),
        "{name}: the output does not match its schema: {problems:#?}\n{output}"
    );
}

#[tokio::test]
async fn outputs_match_their_schemas() {
    let t = TestApp::start().await;
    let f = purchasing(&t).await;
    let c = &f.c;
    let meta = describe(&t).await;

    // Every schema is itself valid, and says something where the output
    // has a fixed shape.
    for command in meta["commands"].as_array().unwrap() {
        let name = command["name"].as_str().unwrap();
        jsonschema::validator_for(&command["output_schema"])
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for name in [
        "journal_entry.post",
        "document.get",
        "purchase_receipt.post",
    ] {
        let schema = &command(&meta, name)["output_schema"];
        let properties = schema["properties"].as_object().unwrap();
        for field in [
            "id", "status", "number", "doc_date", "body", "postings", "links",
        ] {
            assert!(properties.contains_key(field), "{name}: {field}");
        }
    }
    // Decimals are strings, as they are sent.
    let sheet = &command(&meta, "gl.balance_sheet")["output_schema"];
    assert_eq!(sheet["properties"]["net_income"]["type"], "string");
    assert_eq!(sheet["properties"]["company_id"]["format"], "uuid");
    // A generic command returns whatever the entity holds: no promise.
    let get = &command(&meta, "entity.get")["output_schema"];
    assert!(get.get("properties").is_none(), "{get}");

    // Real outputs against the schemas, documents first.
    let draft = t.draft(&t.admin, c, "150.00").await;
    let doc = json!({ "doc_type": "journal_entry", "id": draft["id"] });
    assert_matches(&meta, "document.create", &draft);
    assert_matches(
        &meta,
        "document.get",
        &run(&t, "document.get", doc.clone()).await,
    );
    let posted = run(&t, "journal_entry.post", json!({ "id": draft["id"] })).await;
    assert_matches(&meta, "journal_entry.post", &posted);
    let reversed = run(&t, "journal_entry.reverse", json!({ "id": draft["id"] })).await;
    assert_matches(&meta, "journal_entry.reverse", &reversed);
    let order = f.order(&t, "10", "4.00").await;
    assert_matches(&meta, "purchase_order.approve", &order);
    let (_, receipt) = f.receive(&t, &order, "10", false).await;
    assert_matches(&meta, "purchase_receipt.post", &receipt);
    assert_matches(
        &meta,
        "document.list",
        &run(&t, "document.list", json!({ "doc_type": "journal_entry" })).await,
    );

    // Entities.
    let list = run(&t, "entity.list", json!({ "entity": "account" })).await;
    assert_matches(&meta, "entity.list", &list);
    assert_matches(
        &meta,
        "entity.get",
        &run(
            &t,
            "entity.get",
            json!({ "entity": "account", "id": c.cash }),
        )
        .await,
    );

    // Reports and other queries.
    let company = json!({ "company_id": c.id });
    let range = json!({ "company_id": c.id, "from": "2026-01-01", "to": "2026-12-31" });
    for (name, input) in [
        ("gl.trial_balance", company.clone()),
        ("gl.balance_sheet", company.clone()),
        ("gl.income_statement", range.clone()),
        (
            "gl.account_ledger",
            json!({ "company_id": c.id, "account_id": c.cash }),
        ),
        ("stock.on_hand", company.clone()),
        ("open_item.list", company.clone()),
        ("tax.report", range),
        ("audit.list", json!({})),
        (
            "audit.history",
            json!({ "resource": "journal_entry", "id": draft["id"] }),
        ),
        ("outbox.list", json!({})),
        ("meta.describe", json!({ "module": "gl" })),
        ("fiscal_period.close", json!({ "id": c.period })),
    ] {
        let output = run(&t, name, input).await;
        assert_matches(&meta, name, &output);
    }

    t.stop().await;
}

// ------------------------------------------------------------------ entries

struct Purchasing {
    c: Company,
    item: String,
    warehouse: String,
    vendor: String,
}

/// A company set up to buy widgets into one warehouse.
async fn purchasing(t: &TestApp) -> Purchasing {
    let c = t.company("ACME", "USD").await;
    let item = t
        .create("item", json!({ "code": "WIDGET", "name": "Widget" }))
        .await;
    let warehouse = t
        .create(
            "warehouse",
            json!({ "company_id": c.id, "code": "MAIN", "name": "Main" }),
        )
        .await;
    let vendor = t
        .create("vendor", json!({ "code": "V001", "name": "Parts Inc" }))
        .await;
    let mut accounts = vec![];
    for (code, name, kind) in [
        ("1300", "Inventory", "asset"),
        ("2000", "Accounts payable", "liability"),
        ("2100", "Goods received not invoiced", "liability"),
        ("5200", "Purchase price variance", "expense"),
    ] {
        accounts.push(
            t.create(
                "account",
                json!({ "company_id": c.id, "code": code, "name": name, "account_type": kind }),
            )
            .await,
        );
    }
    t.create(
        "purchasing_settings",
        json!({
            "company_id": c.id,
            "grni_account_id": accounts[2],
            "payable_account_id": accounts[1],
            "price_variance_account_id": accounts[3],
        }),
    )
    .await;
    t.inventory_rule(&c.id, &accounts[0]).await;
    Purchasing {
        c,
        item,
        warehouse,
        vendor,
    }
}

impl Purchasing {
    /// An approved order for `qty` widgets at `price`.
    async fn order(&self, t: &TestApp, qty: &str, price: &str) -> Value {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/purchase_order",
                json!({
                    "company_id": self.c.id,
                    "doc_date": "2026-03-15",
                    "body": {
                        "vendor_id": self.vendor,
                        "lines": [{ "item_id": self.item, "warehouse_id": self.warehouse, "qty": qty, "unit_price": price }],
                    },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        run(t, "purchase_order.approve", json!({ "id": draft["id"] })).await
    }

    /// Draft a receipt against the order and post it, or only preview the
    /// posting. Returns the draft and the outcome of the command.
    async fn receive(
        &self,
        t: &TestApp,
        order: &Value,
        qty: &str,
        preview: bool,
    ) -> (Value, Value) {
        let (status, draft) = t
            .post(
                &t.admin,
                "/api/documents/purchase_receipt",
                json!({
                    "company_id": self.c.id,
                    "doc_date": "2026-03-15",
                    "body": {
                        "purchase_order_id": order["id"],
                        "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": qty }],
                    },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{draft}");
        let path = if preview {
            "/api/commands/purchase_receipt.post?preview=true"
        } else {
            "/api/commands/purchase_receipt.post"
        };
        let (status, outcome) = t.post(&t.admin, path, json!({ "id": draft["id"] })).await;
        assert_eq!(status, StatusCode::OK, "{outcome}");
        (draft, outcome)
    }
}

#[tokio::test]
async fn a_preview_shows_the_ledger_entries_and_saves_nothing() {
    let t = TestApp::start().await;
    let f = purchasing(&t).await;
    let c = &f.c;

    // A journal entry: the preview of posting it shows both entries.
    let draft = t.draft(&t.admin, c, "150.00").await;
    assert_eq!(draft["postings"], json!([]));
    let (status, preview) = t
        .post(
            &t.admin,
            "/api/commands/journal_entry.post?preview=true",
            json!({ "id": draft["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["preview"], true);
    let result = &preview["result"];
    assert_eq!(result["status"], "posted");
    let postings = result["postings"].as_array().unwrap();
    assert_eq!(postings.len(), 1, "{preview}");
    assert_eq!(postings[0]["command"], "journal_entry.post");
    let ledgers: Vec<&String> = postings[0]["entries"].as_object().unwrap().keys().collect();
    assert_eq!(ledgers, ["gl"]);
    let gl = postings[0]["entries"]["gl"].as_array().unwrap();
    assert_eq!(gl.len(), 2, "{preview}");
    assert_eq!(gl[0]["account_id"], json!(c.cash));
    assert_eq!(gl[0]["account_code"], "1000");
    assert_eq!(gl[0]["account_name"], "Cash");
    assert_eq!((dec(&gl[0]["debit"]), dec(&gl[0]["credit"])), (150.0, 0.0));
    assert_eq!(gl[0]["currency"], "USD");
    assert_eq!(gl[1]["account_code"], "4000");
    assert_eq!((dec(&gl[1]["debit"]), dec(&gl[1]["credit"])), (0.0, 150.0));
    // Each entry names the line of the document it comes from.
    assert_eq!(gl[0]["source_line_id"], draft["body"]["lines"][0]["id"]);

    // Nothing was saved.
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM gl_entry").await, 0);
    let path = format!("/api/documents/journal_entry/{}", id(&draft));
    let (_, current) = t.get(&t.admin, &path).await;
    assert_eq!(current["status"], "draft");
    assert_eq!(current["postings"], json!([]));

    // Posting it for real shows the same entries, then and afterwards.
    let posted = run(&t, "journal_entry.post", json!({ "id": draft["id"] })).await;
    let entry = |row: &Value| {
        (
            row["account_code"].clone(),
            row["debit"].clone(),
            row["credit"].clone(),
        )
    };
    let real: Vec<_> = posted["postings"][0]["entries"]["gl"]
        .as_array()
        .unwrap()
        .iter()
        .map(entry)
        .collect();
    assert_eq!(real, gl.iter().map(entry).collect::<Vec<_>>());
    let (_, current) = t.get(&t.admin, &path).await;
    assert_eq!(current["postings"], posted["postings"]);
    // Over MCP too.
    let other = t.draft(&t.admin, c, "20.00").await;
    let tool = t
        .tool(
            &t.admin,
            "journal_entry_post",
            json!({ "id": other["id"], "preview": true }),
        )
        .await;
    let shown = &tool["structuredContent"]["result"]["postings"][0]["entries"]["gl"];
    assert_eq!(shown.as_array().unwrap().len(), 2, "{tool}");
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 1);

    // A purchase receipt writes to two ledgers; the preview shows both.
    let order = f.order(&t, "10", "4.00").await;
    let (receipt, preview) = f.receive(&t, &order, "6", true).await;
    let posting = &preview["result"]["postings"][0];
    let stock = posting["entries"]["stock"].as_array().unwrap();
    assert_eq!(stock.len(), 1, "{preview}");
    assert_eq!(stock[0]["item_id"], json!(f.item));
    assert_eq!(stock[0]["item_code"], "WIDGET");
    assert_eq!(stock[0]["warehouse_code"], "MAIN");
    assert_eq!(dec(&stock[0]["qty"]), 6.0);
    assert_eq!(dec(&stock[0]["value"]), 24.0);
    assert_eq!(dec(&stock[0]["unit_cost"]), 4.0);
    let gl = posting["entries"]["gl"].as_array().unwrap();
    assert_eq!(gl.len(), 2, "{preview}");
    assert_eq!(gl[0]["account_code"], "1300");
    assert_eq!(dec(&gl[0]["debit"]), 24.0);
    assert_eq!(gl[1]["account_code"], "2100");
    assert_eq!(dec(&gl[1]["credit"]), 24.0);
    // Nothing of it was saved: no stock, no entries, the receipt a draft.
    assert_eq!(t.count("SELECT count(*) FROM stock_entry").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM posting").await, 1);
    let on_hand = run(&t, "stock.on_hand", json!({ "company_id": c.id })).await;
    assert_eq!(on_hand["rows"], json!([]));
    let (_, current) = t
        .get(
            &t.admin,
            &format!("/api/documents/purchase_receipt/{}", id(&receipt)),
        )
        .await;
    assert_eq!(current["status"], "draft");

    // An invoice opens an item, and a payment settles it: both are shown.
    run(&t, "purchase_receipt.post", json!({ "id": receipt["id"] })).await;
    let (status, invoice) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({
                "company_id": c.id,
                "doc_date": "2026-03-20",
                "body": {
                    "purchase_order_id": order["id"],
                    "vendor_reference": "INV-77",
                    "lines": [{ "order_line_id": order["body"]["lines"][0]["id"], "qty": "6", "unit_price": "4.00" }],
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    let invoice = run(&t, "vendor_invoice.post", json!({ "id": invoice["id"] })).await;
    let opened = invoice["postings"][0]["entries"]["open_items"]
        .as_array()
        .unwrap();
    assert_eq!(opened.len(), 1, "{invoice}");
    assert_eq!(opened[0]["entry"], "item");
    assert_eq!(opened[0]["kind"], "payable");
    assert_eq!(dec(&opened[0]["amount"]), 24.0);
    let (status, payment) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({
                "company_id": c.id,
                "doc_date": "2026-03-25",
                "body": {
                    "direction": "outgoing",
                    "bank_account_id": c.cash,
                    "allocations": [{ "open_item_id": opened[0]["id"], "amount": "24.00" }],
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{payment}");
    let (_, preview) = t
        .post(
            &t.admin,
            "/api/commands/payment.post?preview=true",
            json!({ "id": payment["id"] }),
        )
        .await;
    let settled = preview["result"]["postings"][0]["entries"]["open_items"]
        .as_array()
        .unwrap();
    assert_eq!(settled.len(), 1, "{preview}");
    assert_eq!(settled[0]["entry"], "settlement");
    assert_eq!(settled[0]["open_item_id"], opened[0]["id"]);
    assert_eq!(settled[0]["number"], invoice["number"]);
    assert_eq!(dec(&settled[0]["amount"]), 24.0);
    assert_eq!(t.count("SELECT count(*) FROM settlement").await, 0);

    t.stop().await;
}

#[tokio::test]
async fn ledger_entries_follow_the_ledgers_permissions() {
    let t = TestApp::start().await;
    let f = purchasing(&t).await;
    let c = &f.c;
    let draft = t.draft(&t.admin, c, "150.00").await;
    run(&t, "journal_entry.post", json!({ "id": draft["id"] })).await;
    let order = f.order(&t, "10", "4.00").await;
    let (receipt, _) = f.receive(&t, &order, "6", false).await;
    let journal = format!("/api/documents/journal_entry/{}", id(&draft));
    let receipt = format!("/api/documents/purchase_receipt/{}", id(&receipt));

    // A reader of the document who may not read the ledger sees the
    // posting, not its entries.
    let reader = t
        .user_with_role(
            "reader@example.com",
            &["journal_entry:read", "purchase_receipt:read"],
            &c.id,
        )
        .await;
    let (status, doc) = t.get(&reader, &journal).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["postings"].as_array().unwrap().len(), 1);
    assert_eq!(doc["postings"][0]["entries"], json!({}));
    let (status, doc) = t.get(&reader, &receipt).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["postings"][0]["entries"], json!({}));

    // With the ledger but not the accounts: entries without names.
    let bookkeeper = t
        .user_with_role(
            "books@example.com",
            &[
                "journal_entry:read",
                "purchase_receipt:read",
                "gl_entry:read",
            ],
            &c.id,
        )
        .await;
    let (_, doc) = t.get(&bookkeeper, &journal).await;
    let gl = doc["postings"][0]["entries"]["gl"].as_array().unwrap();
    assert_eq!(gl.len(), 2);
    assert_eq!(gl[0]["account_id"], json!(c.cash));
    assert_eq!(gl[0]["account_code"], Value::Null);
    assert_eq!(gl[0]["account_name"], Value::Null);
    assert_eq!(dec(&gl[0]["debit"]), 150.0);
    // One ledger, not the other.
    let (_, doc) = t.get(&bookkeeper, &receipt).await;
    let ledgers: Vec<&String> = doc["postings"][0]["entries"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(ledgers, ["gl"]);

    // The value of stock entries is withheld from the storekeeper: the
    // movement is shown, what it is worth is not.
    let (storekeeper, _) = t
        .user_with_rules(
            "store@example.com",
            json!([
                "purchase_receipt:read",
                "item:read",
                "warehouse:read",
                "stock_entry:read",
                { "deny": "stock_entry:read", "fields": ["value", "unit_cost", "value_after", "variance"] },
            ]),
            Some(&c.id),
        )
        .await;
    let (status, doc) = t.get(&storekeeper, &receipt).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    let entries = doc["postings"][0]["entries"].as_object().unwrap();
    assert_eq!(entries.keys().collect::<Vec<_>>(), ["stock"]);
    let row = entries["stock"][0].as_object().unwrap();
    assert_eq!(row["item_code"], "WIDGET");
    assert_eq!(row["warehouse_code"], "MAIN");
    assert_eq!(dec(&row["qty"]), 6.0);
    assert_eq!(dec(&row["qty_after"]), 6.0);
    for field in ["value", "unit_cost", "value_after", "variance"] {
        assert!(!row.contains_key(field), "{field} must be withheld: {doc}");
    }
    // The administrator sees it all.
    let (_, doc) = t.get(&t.admin, &receipt).await;
    assert_eq!(
        dec(&doc["postings"][0]["entries"]["stock"][0]["value"]),
        24.0
    );
    assert_eq!(
        doc["postings"][0]["entries"]["gl"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    t.stop().await;
}

// ------------------------------------------------- what an interface needs

/// Every property of every object anywhere in `schema`, as
/// `path.property`, with its schema.
fn collect_properties<'a>(schema: &'a Value, path: &str, out: &mut Vec<(String, &'a Value)>) {
    match schema {
        Value::Object(map) => {
            for (key, value) in map {
                if key == "properties"
                    && let Some(properties) = value.as_object()
                {
                    for (name, property) in properties {
                        let at = format!("{path}.{name}");
                        out.push((at.clone(), property));
                        collect_properties(property, &at, out);
                    }
                } else {
                    collect_properties(value, &format!("{path}/{key}"), out);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_properties(item, path, out);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn report_output_properties_are_titled() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let open_items = command(&meta, "open_item.list");
    let item = open_items["output_schema"]
        .pointer("/$defs/ListedItem/properties/number/title")
        .or_else(|| {
            open_items["output_schema"].pointer("/properties/items/items/properties/number/title")
        });
    assert_eq!(item, Some(&json!("Number")), "{open_items}");

    let mut missing = vec![];
    let mut reports = 0;
    for c in meta["commands"].as_array().unwrap() {
        if c["report"].is_null() {
            continue;
        }
        reports += 1;
        let mut found = vec![];
        collect_properties(&c["output_schema"], c["name"].as_str().unwrap(), &mut found);
        assert!(!found.is_empty(), "{} has no output properties", c["name"]);
        for (at, schema) in found {
            if schema.is_object() && !schema["title"].is_string() {
                missing.push(at);
            }
        }
    }
    assert!(reports >= 8, "only {reports} reports");
    assert!(
        missing.is_empty(),
        "report output properties without a title: {missing:#?}"
    );
    t.stop().await;
}

#[tokio::test]
async fn rule_errors_name_the_field_and_the_rule() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // A policy condition that does not parse, or reads a fact the point
    // does not supply.
    for condition in ["facts.qty >", "facts.no_such_fact == 1"] {
        let (status, body) = t
            .post(
                &t.admin,
                "/api/entities/policy_rule",
                json!({ "company_id": c.id, "point": "stock.negative", "effect": "allow", "condition": condition }),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{condition}: {body}"
        );
        assert_eq!(error(&body)["field"], "condition", "{condition}: {body}");
    }
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/policy_rule",
            json!({ "point": "no.such_point", "effect": "allow", "condition": null }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "point");

    // The second rule of a role refers to a field the entity lacks.
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/role",
            json!({ "name": "Clerk", "permissions": [
                "account:read",
                { "allow": "account:read", "where": "record.no_such_field == 1" },
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "permissions", "{body}");
    assert_eq!(error(&body)["line"], 2, "{body}");
    let (status, body) = t
        .post(
            &t.admin,
            "/api/entities/role",
            json!({ "name": "Clerk", "permissions": [{ "allow": "account:read", "where": "record.code ==" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "permissions", "{body}");
    assert_eq!(error(&body)["line"], 1, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn entity_commands_say_which_states_they_apply_from() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let period = meta["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "fiscal_period")
        .unwrap();
    assert_eq!(period["state_field"], "status");
    assert_eq!(
        command(&meta, "fiscal_period.close")["from_states"],
        json!(["open"])
    );
    assert_eq!(
        command(&meta, "fiscal_period.reopen")["from_states"],
        json!(["closed"])
    );
    // An entity without states has none to name.
    for name in ["sso_provider.update", "webhook.update"] {
        assert_eq!(command(&meta, name)["from_states"], json!([]), "{name}");
    }
    t.stop().await;
}

#[tokio::test]
async fn a_settled_open_item_can_still_be_shown_by_its_id() {
    let t = TestApp::start().await;
    let p = purchasing(&t).await;
    let meta = describe(&t).await;

    // The payment's allocation names how to read one item by its id.
    let payment = meta["doc_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "payment")
        .unwrap();
    let field = payment["collections"][0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "open_item_id")
        .unwrap();
    assert_eq!(field["type"]["kind"], "lookup", "{field}");
    assert_eq!(field["type"]["command"], "open_item.list");
    assert_eq!(field["type"]["by_keys"], "ids", "{field}");

    // An invoice, paid in full.
    let order = p.order(&t, "2", "5.00").await;
    p.receive(&t, &order, "2", false).await;
    let line = &order["body"]["lines"][0]["id"];
    let (status, invoice) = t
        .post(
            &t.admin,
            "/api/documents/vendor_invoice",
            json!({
                "company_id": p.c.id, "doc_date": "2026-03-15",
                "body": { "purchase_order_id": order["id"], "lines": [{ "order_line_id": line, "qty": "2", "unit_price": "5.00" }] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    run(&t, "vendor_invoice.post", json!({ "id": invoice["id"] })).await;
    let open = run(
        &t,
        "open_item.list",
        json!({ "company_id": p.c.id, "kind": "payable" }),
    )
    .await;
    let item = open["items"][0]["id"].clone();
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({
                "company_id": p.c.id, "doc_date": "2026-03-20",
                "body": { "direction": "outgoing", "bank_account_id": p.c.cash,
                          "allocations": [{ "open_item_id": item, "amount": "10.00" }] },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{draft}");
    run(&t, "payment.post", json!({ "id": draft["id"] })).await;

    // Settled, it is gone from the list of what is open...
    let open = run(
        &t,
        "open_item.list",
        json!({ "company_id": p.c.id, "kind": "payable" }),
    )
    .await;
    assert_eq!(open["total"], 0);
    // ...but the payment can still show it, by its id.
    let shown = run(
        &t,
        "open_item.list",
        json!({ "company_id": p.c.id, "kind": "payable", "ids": [item] }),
    )
    .await;
    assert_eq!(shown["total"], 1, "{shown}");
    assert_eq!(shown["items"][0]["id"], item);
    assert_eq!(dec(&shown["items"][0]["open"]), 0.0);
    assert_eq!(
        shown["items"][0]["number"],
        invoice_number(&t, &invoice).await
    );
    // Only the ids asked for.
    let none = run(
        &t,
        "open_item.list",
        json!({ "company_id": p.c.id, "ids": [uuid_nil()] }),
    )
    .await;
    assert_eq!(none["total"], 0);
    t.stop().await;
}

async fn invoice_number(t: &TestApp, invoice: &Value) -> Value {
    let (_, doc) = t
        .get(
            &t.admin,
            &format!("/api/documents/vendor_invoice/{}", id(invoice)),
        )
        .await;
    doc["number"].clone()
}

fn uuid_nil() -> &'static str {
    "00000000-0000-0000-0000-000000000000"
}

// --------------------------------------- ledgers, written together, tones

#[tokio::test]
async fn ledger_entries_have_a_schema_of_their_own() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let ledgers = meta["ledgers"].as_array().unwrap();
    let names: Vec<&str> = ledgers
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    for name in ["gl", "stock", "open_items", "tax"] {
        assert!(names.contains(&name), "{name}: {names:?}");
    }
    for ledger in ledgers {
        let properties = ledger["entry_schema"]["properties"].as_object().unwrap();
        assert!(properties.len() >= 5, "{ledger}");
        for (name, property) in properties {
            assert!(property["title"].is_string(), "{}.{name}", ledger["name"]);
        }
    }
    let gl = ledgers.iter().find(|l| l["name"] == "gl").unwrap();
    assert_eq!(gl["label"], "General ledger");
    assert_eq!(gl["entry_schema"]["properties"]["debit"]["title"], "Debit");

    // Every command that returns a document says what its entries hold.
    let mut with_postings = 0;
    for c in meta["commands"].as_array().unwrap() {
        let Some(entries) = c["output_schema"].pointer("/$defs/PostingView/properties/entries")
        else {
            continue;
        };
        with_postings += 1;
        assert_eq!(
            entries["properties"]["gl"]["items"]["properties"]["account_code"]["title"],
            "Account code",
            "{}",
            c["name"]
        );
    }
    assert!(with_postings >= 20, "{with_postings}");
    t.stop().await;
}

#[tokio::test]
async fn fields_checked_against_each_other_are_written_together() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let meta = describe(&t).await;
    let entity = |name: &str| {
        meta["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        entity("policy_rule")["written_together"],
        json!([["point", "condition"]])
    );
    assert_eq!(
        entity("account_rule")["written_together"],
        json!([["scope_type", "scope_id"]])
    );
    assert_eq!(entity("account")["written_together"], json!([]));

    let rule = t
        .create(
            "policy_rule",
            json!({ "company_id": c.id, "point": "stock.negative", "effect": "allow", "condition": null }),
        )
        .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{rule}"),
            json!({ "condition": "facts.short_by < 10" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(error(&body)["field"], "point", "{body}");
    assert!(
        error(&body)["message"]
            .as_str()
            .unwrap()
            .contains("together with 'condition'")
    );
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{rule}"),
            json!({ "point": "stock.negative", "condition": "facts.short_by < 10" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Fields outside the group change on their own, as before.
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/policy_rule/{rule}"),
            json!({ "message": "only a few" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    t.stop().await;
}

#[tokio::test]
async fn states_say_how_to_show_them() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let tone = |doc_type: &str, state: &str| {
        meta["doc_types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == doc_type)
            .unwrap()["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == state)
            .unwrap()["tone"]
            .clone()
    };
    assert_eq!(tone("journal_entry", "draft"), "neutral");
    assert_eq!(tone("journal_entry", "posted"), "success");
    assert_eq!(tone("journal_entry", "reversed"), "danger");
    assert_eq!(tone("purchase_order", "open"), "info");
    assert_eq!(tone("purchase_order", "closed"), "success");
    assert_eq!(tone("purchase_order", "cancelled"), "neutral");
    for d in meta["doc_types"].as_array().unwrap() {
        for s in d["states"].as_array().unwrap() {
            assert!(
                ["neutral", "info", "success", "warning", "danger"]
                    .contains(&s["tone"].as_str().unwrap()),
                "{}.{}",
                d["name"],
                s["name"]
            );
        }
    }
    t.stop().await;
}

#[tokio::test]
async fn a_payment_offers_bank_accounts_only() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    let payment = meta["doc_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "payment")
        .unwrap();
    let bank = payment["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "bank_account_id")
        .unwrap();
    assert_eq!(
        bank["type"]["filters"],
        json!({ "is_bank": "true" }),
        "{bank}"
    );
    // A link without a filter is published as before.
    let account = meta["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "account")
        .unwrap();
    let parent = account["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "parent_id")
        .unwrap();
    assert_eq!(
        parent["type"],
        json!({ "kind": "link", "entity": "account", "key": "id" })
    );

    // What the filter finds: the bank and cash accounts a setup marked.
    let (status, created) = t
        .command(
            &t.admin,
            "company.setup",
            json!({ "template": "de_skr03", "code": "DE", "name": "DE", "first_year": 2026 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let listed = run(
        &t,
        "entity.list",
        json!({ "entity": "account", "filters": { "company_id": created["company"]["id"], "is_bank": "true" } }),
    )
    .await;
    let mut codes: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["code"].as_str().unwrap())
        .collect();
    codes.sort();
    assert_eq!(codes, ["1000", "1200"]);

    // A control account is never a bank account, either way round.
    let receivable = created["roles"]["receivable"]["id"].as_str().unwrap();
    let (status, body) = t
        .patch(
            &t.admin,
            &format!("/api/entities/account/{receivable}"),
            json!({ "is_bank": true }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        error(&body)["message"]
            .as_str()
            .unwrap()
            .contains("cannot be a bank or cash account"),
        "{body}"
    );
    let settings = run(
        &t,
        "entity.list",
        json!({ "entity": "sales_settings", "filters": { "company_id": created["company"]["id"] } }),
    )
    .await;
    let (status, body) = t
        .patch(
            &t.admin,
            &format!(
                "/api/entities/sales_settings/{}",
                settings["items"][0]["id"].as_str().unwrap()
            ),
            json!({ "receivable_account_id": created["roles"]["bank"]["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        error(&body)["message"]
            .as_str()
            .unwrap()
            .contains("cannot be a control account"),
        "{body}"
    );

    // An account a posted payment already went through is marked by the
    // upgrade.
    let c = t.company("ACME", "USD").await;
    let (status, draft) = t
        .post(
            &t.admin,
            "/api/documents/payment",
            json!({ "company_id": c.id, "doc_date": "2026-03-15",
                    "body": { "direction": "outgoing", "bank_account_id": c.cash } }),
        )
        .await;
    if status == StatusCode::CREATED {
        // Posting may need allocations; the mark only needs a posting row.
        sqlx::query(
            "INSERT INTO posting (id, company_id, document_id, command, posting_date, created_by)
             SELECT gen_random_uuid(), d.company_id, d.id, 'test', d.doc_date, d.created_by
             FROM document d WHERE d.id = $1",
        )
        .bind(uuid::Uuid::parse_str(&id(&draft)).unwrap())
        .execute(&t.pool)
        .await
        .unwrap();
        // The marking statement of the upgrade: its first.
        let upgrade = include_str!("../../../modules/payments/migrations/0003_bank_accounts.sql");
        let mark = upgrade
            .split(';')
            .next()
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::query(&mark).execute(&t.pool).await.unwrap();
        assert_eq!(
            t.count(&format!(
                "SELECT count(*) FROM account WHERE is_bank AND id = '{}'",
                c.cash
            ))
            .await,
            1
        );
        assert_eq!(
            t.count(&format!(
                "SELECT count(*) FROM account WHERE is_bank AND company_id = '{}'",
                c.id
            ))
            .await,
            1
        );
    } else {
        panic!("a payment draft could not be made: {draft}");
    }
    t.stop().await;
}

/// A secret a command takes is marked `writeOnly` in its schema, so that a
/// form shows a password field and never fills it from a record.
#[tokio::test]
async fn secret_inputs_are_write_only() {
    let t = TestApp::start().await;
    let meta = describe(&t).await;
    for (name, property) in [
        ("user.create", "password"),
        ("sso_provider.create", "client_secret"),
        ("sso_provider.set_secret", "client_secret"),
        ("webhook.create", "secret"),
        ("webhook.set_secret", "secret"),
    ] {
        let schema = &command(&meta, name)["input_schema"]["properties"][property];
        assert_eq!(schema["writeOnly"], true, "{name}.{property}: {schema}");
    }
    // And any other input named like a secret.
    let mut open = vec![];
    for c in meta["commands"].as_array().unwrap() {
        let Some(properties) = c["input_schema"]["properties"].as_object() else {
            continue;
        };
        for (property, schema) in properties {
            let secret = ["password", "secret", "api_key", "private_key"]
                .iter()
                .any(|s| property.contains(s));
            if secret && schema["writeOnly"] != true {
                open.push(format!("{}.{property}", c["name"].as_str().unwrap()));
            }
        }
    }
    assert!(open.is_empty(), "secret inputs not writeOnly: {open:?}");
    t.stop().await;
}
