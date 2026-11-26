//! The MCP endpoint, held against revision 2026-07-28 of the Model Context
//! Protocol specification and the revisions before it.
//!
//! No official client could be run here (they are distributed as packages
//! to download and execute), so these tests speak JSON-RPC over HTTP
//! directly, as the specification writes it down.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{Company, TestApp, id};
use erpcraft_mcp::Config;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const CURRENT: &str = "2026-07-28";
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

/// A request as a client of the current revision sends it: the revision
/// and the client's capabilities in `_meta`, mirrored in the headers.
async fn modern(t: &TestApp, token: &str, method: &str, mut params: Value) -> (StatusCode, Value) {
    params["_meta"] = json!({
        META_VERSION: CURRENT,
        "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" },
        META_CAPABILITIES: {},
    });
    let name = params
        .get("name")
        .or(params.get("uri"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut headers = vec![
        ("accept", "application/json, text/event-stream"),
        ("mcp-protocol-version", CURRENT),
        ("mcp-method", method),
    ];
    if let Some(name) = &name {
        headers.push(("mcp-name", name));
    }
    t.send(
        "POST",
        "/mcp",
        Some(token),
        Some(json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })),
        &headers,
    )
    .await
}

/// The result of a request of the current revision that must succeed.
async fn result(t: &TestApp, token: &str, method: &str, params: Value) -> Value {
    let (status, body) = modern(t, token, method, params).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], 7);
    assert!(body.get("error").is_none(), "{body}");
    assert_eq!(body["result"]["resultType"], "complete", "{body}");
    assert_eq!(body["result"]["_meta"][META_SERVER]["name"], "erpcraft");
    body["result"].clone()
}

/// A request as a client of an earlier revision sends it after
/// `initialize`: no `_meta`, and the negotiated revision in a header (or
/// no header at all, before 2025-06-18).
async fn legacy(
    t: &TestApp,
    token: &str,
    version: Option<&str>,
    method: &str,
    params: Value,
) -> (StatusCode, Value) {
    let mut headers = vec![("accept", "application/json, text/event-stream")];
    if let Some(version) = version {
        headers.push(("mcp-protocol-version", version));
    }
    t.send(
        "POST",
        "/mcp",
        Some(token),
        Some(json!({ "jsonrpc": "2.0", "id": "a", "method": method, "params": params })),
        &headers,
    )
    .await
}

/// Call a tool as a client of the current revision.
async fn call(t: &TestApp, token: &str, name: &str, arguments: Value) -> Value {
    result(
        t,
        token,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await
}

/// Every tool the caller is shown, following the cursor to the end.
async fn all_tools(t: &TestApp, token: &str) -> Vec<Value> {
    let mut tools = vec![];
    let mut cursor = Value::Null;
    loop {
        let page = result(t, token, "tools/list", json!({ "cursor": cursor })).await;
        tools.extend(page["tools"].as_array().unwrap().iter().cloned());
        match page.get("nextCursor") {
            Some(next) => cursor = next.clone(),
            None => return tools,
        }
    }
}

fn named<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

fn names(tools: &[Value]) -> Vec<&str> {
    tools.iter().map(|t| t["name"].as_str().unwrap()).collect()
}

/// Check a value against a JSON Schema, as a client should.
fn conforms(schema: &Value, value: &Value, what: &str) {
    let validator =
        jsonschema::validator_for(schema).unwrap_or_else(|e| panic!("{what}: bad schema: {e}"));
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{what}: {errors:?}\n{value}");
}

/// A clerk who may draft and read journal entries, and read accounts.
async fn clerk(t: &TestApp, c: &Company) -> String {
    t.user_with_role(
        "clerk@example.com",
        &["journal_entry:create", "journal_entry:read", "account:read"],
        &c.id,
    )
    .await
}

// ------------------------------------------------------------- versions

#[tokio::test]
async fn the_current_revision_is_served_without_a_handshake() {
    let t = TestApp::start().await;

    // What the server is, asked the way the current revision asks.
    let discovered = result(&t, &t.admin, "server/discover", json!({})).await;
    assert_eq!(
        discovered["supportedVersions"],
        json!([
            "2026-07-28",
            "2025-11-25",
            "2025-06-18",
            "2025-03-26",
            "2024-11-05"
        ])
    );
    assert_eq!(
        discovered["capabilities"],
        json!({
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
            "prompts": { "listChanged": false },
        })
    );
    assert_eq!(discovered["cacheScope"], "public");
    assert!(discovered["ttlMs"].as_u64().unwrap() > 0);
    assert!(discovered["_meta"][META_SERVER]["version"].is_string());
    // The guide an agent reads first says the things that matter.
    let instructions = discovered["instructions"].as_str().unwrap();
    for said in [
        "erpcraft_modules",
        "erpcraft_tools",
        "permissions of the signed-in user",
        "scopes of the token",
        "preview: true",
        "idempotency_key",
        "decimal strings",
        "structuredContent.error",
        "`field`",
        "`line`",
        "`reason`",
    ] {
        assert!(instructions.contains(said), "instructions lack: {said}");
    }
    assert!(instructions.len() < 2_000, "the instructions are short");

    // Any request works without one before it: nothing is kept.
    let tools = result(&t, &t.admin, "tools/list", json!({})).await;
    assert_eq!(
        tools["cacheScope"], "private",
        "the list depends on the caller"
    );
    assert!(tools["ttlMs"].as_u64().unwrap() > 0);

    let send = |method: &'static str, params: Value, headers: Vec<(&'static str, &'static str)>| {
        let t = &t;
        async move {
            t.send(
                "POST",
                "/mcp",
                Some(&t.admin),
                Some(json!({ "jsonrpc": "2.0", "id": 3, "method": method, "params": params })),
                &headers,
            )
            .await
        }
    };
    let meta = |version: &str| json!({ META_VERSION: version, META_CAPABILITIES: {} });

    // A revision the server does not have: it says which it has.
    let (status, body) = send(
        "tools/list",
        json!({ "_meta": meta("2027-01-01") }),
        vec![
            ("mcp-protocol-version", "2027-01-01"),
            ("mcp-method", "tools/list"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["id"], 3);
    assert_eq!(body["error"]["code"], -32022);
    assert_eq!(body["error"]["data"]["requested"], "2027-01-01");
    assert_eq!(body["error"]["data"]["supported"][0], CURRENT);

    // The headers must be there and must say what the body says.
    for (headers, what) in [
        (
            vec![("mcp-method", "tools/call"), ("mcp-name", "ping")],
            "MCP-Protocol-Version",
        ),
        (
            vec![
                ("mcp-protocol-version", "2025-11-25"),
                ("mcp-method", "tools/call"),
                ("mcp-name", "x"),
            ],
            "MCP-Protocol-Version",
        ),
        (
            vec![("mcp-protocol-version", CURRENT), ("mcp-name", "x")],
            "Mcp-Method",
        ),
        (
            vec![
                ("mcp-protocol-version", CURRENT),
                ("mcp-method", "tools/list"),
                ("mcp-name", "x"),
            ],
            "Mcp-Method",
        ),
        (
            vec![
                ("mcp-protocol-version", CURRENT),
                ("mcp-method", "tools/call"),
            ],
            "Mcp-Name",
        ),
        (
            vec![
                ("mcp-protocol-version", CURRENT),
                ("mcp-method", "tools/call"),
                ("mcp-name", "meta_describe"),
            ],
            "Mcp-Name",
        ),
        (
            vec![
                ("mcp-protocol-version", CURRENT),
                ("mcp-method", "tools/call"),
                ("mcp-name", "=?base64?!!?="),
            ],
            "Mcp-Name",
        ),
    ] {
        let (status, body) = send(
            "tools/call",
            json!({ "name": "erpcraft_modules", "arguments": {}, "_meta": meta(CURRENT) }),
            headers,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], -32020, "{body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(what),
            "{body}"
        );
    }
    // A name sent in the Base64 form of the specification is decoded.
    let (status, body) = send(
        "tools/call",
        json!({ "name": "erpcraft_modules", "arguments": {}, "_meta": meta(CURRENT) }),
        vec![
            ("mcp-protocol-version", CURRENT),
            ("mcp-method", "tools/call"),
            ("mcp-name", "=?base64?ZXJwY3JhZnRfbW9kdWxlcw==?="),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["isError"], false);

    // The required metadata must be in the body.
    let headers = || {
        vec![
            ("mcp-protocol-version", CURRENT),
            ("mcp-method", "tools/list"),
        ]
    };
    for params in [
        json!({}),
        json!({ "_meta": { META_VERSION: CURRENT } }),
        json!({ "_meta": { META_VERSION: CURRENT, META_CAPABILITIES: "all" } }),
    ] {
        let (status, body) = send("tools/list", params, headers()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], -32602, "{body}");
    }

    // A method the server does not have is 404 with a JSON-RPC error, so
    // a client can tell it from an endpoint that is not there. `ping` and
    // `initialize` are not methods of this revision.
    for method in ["ping", "subscriptions/listen", "completion/complete"] {
        let (status, body) = modern(&t, &t.admin, method, json!({})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], -32601, "{body}");
    }

    t.stop().await;
}

#[tokio::test]
async fn earlier_revisions_are_served_after_initialize() {
    let t = TestApp::start().await;
    let initialize = |version: &'static str| {
        let t = &t;
        async move {
            let (status, body) = legacy(
                t,
                &t.admin,
                None,
                "initialize",
                json!({
                    "protocolVersion": version,
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" },
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["result"].clone()
        }
    };

    // A revision the server has is taken; for any other it offers the
    // newest of those that open with `initialize`.
    for (asked, answered) in [
        ("2025-11-25", "2025-11-25"),
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("2024-11-05", "2024-11-05"),
        ("2023-01-01", "2025-11-25"),
        // The current revision has no `initialize`.
        ("2026-07-28", "2025-11-25"),
    ] {
        let init = initialize(asked).await;
        assert_eq!(init["protocolVersion"], answered, "asked {asked}");
        assert_eq!(init["serverInfo"]["name"], "erpcraft");
        assert!(init["capabilities"]["tools"].is_object());
        assert!(init["capabilities"]["resources"].is_object());
        assert!(init["capabilities"]["prompts"].is_object());
        assert!(init["instructions"].as_str().unwrap().contains("preview"));
        // Results of these revisions have none of the newer fields.
        assert!(init.get("resultType").is_none());
    }

    // The notification that ends the handshake is accepted, with no body.
    let (status, body) = t
        .send(
            "POST",
            "/mcp",
            Some(&t.admin),
            Some(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
            &[("mcp-protocol-version", "2025-11-25")],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body, Value::Null);
    let (status, body) = legacy(&t, &t.admin, Some("2025-11-25"), "ping", json!({})).await;
    assert_eq!((status, &body["result"]), (StatusCode::OK, &json!({})));

    // Each revision is sent what it knows.
    let listed = |version: Option<&'static str>| {
        let t = &t;
        async move {
            // Through every page: the list is longer than one.
            let mut tools = vec![];
            let mut cursor = Value::Null;
            loop {
                let (status, body) = legacy(
                    t,
                    &t.admin,
                    version,
                    "tools/list",
                    json!({ "cursor": cursor }),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{body}");
                let result = &body["result"];
                for newer in ["resultType", "ttlMs", "cacheScope", "_meta"] {
                    assert!(result.get(newer).is_none(), "{version:?} got {newer}");
                }
                tools.extend(result["tools"].as_array().unwrap().iter().cloned());
                match result.get("nextCursor") {
                    Some(next) => cursor = next.clone(),
                    None => break,
                }
            }
            named(&tools, "journal_entry_post").clone()
        }
    };
    for version in ["2025-11-25", "2025-06-18"] {
        let tool = listed(Some(version)).await;
        assert_eq!(tool["title"], "Post journal entry");
        assert_eq!(tool["outputSchema"]["type"], "object");
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
    }
    // Without the header a client is taken to speak 2025-03-26, which has
    // annotations and neither titles nor output schemas.
    for version in [None, Some("2025-03-26")] {
        let tool = listed(version).await;
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert!(tool.get("title").is_none() && tool.get("outputSchema").is_none());
    }
    let tool = listed(Some("2024-11-05")).await;
    assert!(tool.get("annotations").is_none());
    assert!(tool["inputSchema"]["properties"]["id"].is_object());

    // A tool call is the same in every revision, structured content
    // included.
    let (status, body) = legacy(
        &t,
        &t.admin,
        Some("2025-06-18"),
        "tools/call",
        json!({ "name": "erpcraft_modules" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], "a");
    assert_eq!(body["result"]["isError"], false);
    assert!(body["result"]["structuredContent"]["modules"].is_array());

    // A header naming a revision the server does not have is refused, and
    // an unknown method is an error in the body, as it always was.
    let (status, body) = legacy(&t, &t.admin, Some("2022-02-02"), "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["data"]["supported"][1], "2025-11-25");
    let (status, body) = legacy(
        &t,
        &t.admin,
        Some("2025-11-25"),
        "server/discover",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32601, "{body}");

    t.stop().await;
}

// ---------------------------------------------------------------- tools

#[tokio::test]
async fn tools_are_listed_in_pages_and_only_those_the_caller_can_run() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // The first page starts with the two tools for finding one's way.
    let first = result(&t, &t.admin, "tools/list", json!({})).await;
    let page = first["tools"].as_array().unwrap();
    assert_eq!(names(page)[..2], ["erpcraft_modules", "erpcraft_tools"]);
    assert_eq!(page.len(), 50);
    let cursor = first["nextCursor"].as_str().expect("there is more");

    // The pages together are every command, once, in an order that does
    // not change from one request to the next.
    let all = all_tools(&t, &t.admin).await;
    let commands = t.kernel.registry().commands().count();
    assert!(commands > 60, "the list is long enough to need paging");
    assert_eq!(all.len(), commands + 2);
    let mut unique = names(&all);
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), all.len(), "no tool twice");
    let mut by_name: Vec<String> = t
        .kernel
        .registry()
        .commands()
        .map(|entry| entry.meta.tool_name())
        .collect();
    by_name.sort();
    assert_eq!(
        names(&all)[2..],
        by_name,
        "in the order of the tools' names"
    );
    assert_eq!(names(&all), names(&all_tools(&t, &t.admin).await));
    // A cursor goes on exactly where its page ended.
    let second = result(&t, &t.admin, "tools/list", json!({ "cursor": cursor })).await;
    assert_eq!(second["tools"][0], all[50]);
    assert_eq!(second["cacheScope"], "private");

    // A cursor the server did not give out is an error, not a first page.
    for cursor in [json!("no_such_tool"), json!(50)] {
        let (status, body) = modern(&t, &t.admin, "tools/list", json!({ "cursor": cursor })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602, "{body}");
    }

    // A clerk is shown what a clerk could run, on one page.
    let clerk = clerk(&t, &c).await;
    let listed = all_tools(&t, &clerk).await;
    let shown = names(&listed);
    assert!(listed.len() < 40, "{shown:?}");
    for name in [
        "erpcraft_modules",
        "erpcraft_tools",
        "document_create",
        "meta_describe",
    ] {
        assert!(shown.contains(&name), "{name} is missing");
    }
    for name in [
        "journal_entry_post",
        "user_create",
        "fiscal_period_close",
        "gl_trial_balance",
    ] {
        assert!(!shown.contains(&name), "{name} is shown");
    }
    // Calling a tool that is not shown is refused by the command itself.
    let draft = t.draft(&clerk, &c, "10.00").await;
    let refused = call(
        &t,
        &clerk,
        "journal_entry_post",
        json!({ "id": id(&draft) }),
    )
    .await;
    assert_eq!(refused["isError"], true);
    assert_eq!(refused["structuredContent"]["error"]["code"], "forbidden");

    t.stop().await;
}

#[tokio::test]
async fn tools_describe_themselves_from_their_commands() {
    let t = TestApp::start().await;
    let tools = all_tools(&t, &t.admin).await;

    // Every tool: a name the specification allows, a title, hints, and
    // schemas a validator accepts.
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        assert!(
            (1..=128).contains(&name.len())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)),
            "{name}"
        );
        assert!(!tool["title"].as_str().unwrap().is_empty(), "{name}");
        assert!(tool["description"].as_str().unwrap().len() > 20, "{name}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert_eq!(tool["outputSchema"]["type"], "object", "{name}");
        for schema in ["inputSchema", "outputSchema"] {
            assert!(tool[schema].get("$schema").is_none(), "{name}");
            jsonschema::validator_for(&tool[schema])
                .unwrap_or_else(|e| panic!("{name} {schema}: {e}"));
        }
        let hints = &tool["annotations"];
        assert_eq!(hints["openWorldHint"], false, "{name}");
        let properties = &tool["inputSchema"]["properties"];
        if hints["readOnlyHint"] == true {
            // The hints about changes mean nothing for a tool that makes none.
            assert!(hints.get("destructiveHint").is_none(), "{name}");
            assert!(properties.get("preview").is_none(), "{name}");
            assert!(properties.get("idempotency_key").is_none(), "{name}");
        } else {
            assert!(hints["destructiveHint"].is_boolean(), "{name}");
            assert!(hints["idempotentHint"].is_boolean(), "{name}");
            assert_eq!(properties["preview"]["type"], "boolean", "{name}");
            assert_eq!(properties["idempotency_key"]["type"], "string", "{name}");
        }
    }

    // A sample, against what the commands declare.
    for (name, title, read_only, destructive, idempotent) in [
        ("gl_trial_balance", "Trial balance", true, None, None),
        ("entity_list", "List (Entity)", true, None, None),
        (
            "journal_entry_post",
            "Post journal entry",
            false,
            Some(false),
            Some(true),
        ),
        (
            "journal_entry_reverse",
            "Reverse journal entry",
            false,
            Some(true),
            Some(true),
        ),
        (
            "purchase_order_cancel",
            "Cancel purchase order",
            false,
            Some(true),
            Some(true),
        ),
        // Applies from a state, so running it twice changes nothing more.
        (
            "fiscal_period_close",
            "Close fiscal period",
            false,
            Some(false),
            Some(true),
        ),
        // Creating twice makes two: only a key makes a retry safe.
        (
            "document_create",
            "Create (Document)",
            false,
            Some(false),
            Some(false),
        ),
        (
            "document_delete",
            "Delete (Document)",
            false,
            Some(true),
            Some(false),
        ),
    ] {
        let tool = named(&tools, name);
        let meta = &t.kernel.registry().command_by_tool(name).unwrap().meta;
        assert_eq!(tool["title"], title, "{name}");
        assert_eq!(tool["description"], meta.description.as_str(), "{name}");
        let hints = &tool["annotations"];
        assert_eq!(hints["readOnlyHint"], read_only, "{name}");
        assert_eq!(hints["readOnlyHint"], meta.read_only, "{name}");
        assert_eq!(
            hints.get("destructiveHint").and_then(Value::as_bool),
            destructive,
            "{name}"
        );
        assert_eq!(
            hints.get("idempotentHint").and_then(Value::as_bool),
            idempotent,
            "{name}"
        );
        if !read_only {
            assert_eq!(hints["destructiveHint"], meta.destructive, "{name}");
        }
    }

    // What the injected arguments are for is said where they are defined,
    // in a line: they are in every tool an agent loads, and the server's
    // instructions say when to use them.
    let post = &named(&tools, "journal_entry_post")["inputSchema"]["properties"];
    let preview = post["preview"]["description"].as_str().unwrap();
    assert!(preview.contains("save nothing"), "{preview}");
    let key = post["idempotency_key"]["description"].as_str().unwrap();
    assert!(
        key.contains("retry") && key.contains("second change"),
        "{key}"
    );
    let reverse = &named(&tools, "journal_entry_reverse")["inputSchema"]["properties"];
    assert!(
        reverse["preview"]["description"]
            .as_str()
            .unwrap()
            .contains("always preview it first")
    );

    t.stop().await;
}

#[tokio::test]
async fn a_structured_result_conforms_to_the_output_schema() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;
    let tools = all_tools(&t, &t.admin).await;
    let draft = t.draft(&t.admin, &c, "100.00").await;

    let checked = |name: &'static str, arguments: Value| {
        let (t, tools) = (&t, &tools);
        async move {
            let result = call(t, &t.admin, name, arguments).await;
            assert_eq!(result["isError"], false, "{name}: {result}");
            let structured = &result["structuredContent"];
            conforms(&named(tools, name)["outputSchema"], structured, name);
            // The text is the same, for a client that reads only text.
            assert_eq!(result["content"][0]["type"], "text");
            let text: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(&text, structured, "{name}");
            structured.clone()
        }
    };

    // Reports and lists.
    let balance = checked("gl_trial_balance", json!({ "company_id": c.id })).await;
    assert!(balance["rows"].is_array());
    let accounts = checked("entity_list", json!({ "entity": "account" })).await;
    assert_eq!(accounts["total"], 3);
    checked("entity_get", json!({ "entity": "account", "id": c.cash })).await;
    checked("document_list", json!({ "doc_type": "journal_entry" })).await;
    checked("meta_describe", json!({ "module": "accounting" })).await;
    checked(
        "audit_history",
        json!({ "resource": "journal_entry", "id": id(&draft) }),
    )
    .await;
    checked("erpcraft_modules", json!({})).await;
    checked("erpcraft_tools", json!({ "module": "gl" })).await;

    // A change, previewed and then made. The two results differ in shape,
    // and the schema of a tool that writes allows for both.
    let preview = checked(
        "journal_entry_post",
        json!({ "id": id(&draft), "preview": true }),
    )
    .await;
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["result"]["status"], "posted");
    let (_, unchanged) = t
        .get(
            &t.admin,
            &format!("/api/documents/journal_entry/{}", id(&draft)),
        )
        .await;
    assert_eq!(unchanged["status"], "draft", "a preview saves nothing");
    let posted = checked(
        "journal_entry_post",
        json!({ "id": id(&draft), "idempotency_key": "post-1" }),
    )
    .await;
    assert_eq!(posted["status"], "posted");
    assert!(posted.get("preview").is_none());
    // The key makes the retry return the first result.
    let again = checked(
        "journal_entry_post",
        json!({ "id": id(&draft), "idempotency_key": "post-1" }),
    )
    .await;
    assert_eq!(again, posted);

    // A schema that is wrong for the result is told apart: the check
    // above is not one that passes anything.
    let schema = &named(&tools, "gl_trial_balance")["outputSchema"];
    let validator = jsonschema::validator_for(schema).unwrap();
    assert!(!validator.is_valid(&json!({ "rows": "none" })));
    assert!(!validator.is_valid(&posted));

    t.stop().await;
}

#[tokio::test]
async fn a_failed_tool_says_what_was_wrong_and_where() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // A line of a document body that cannot be saved.
    let failed = call(
        &t,
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
    assert_eq!(failed["isError"], true, "{failed}");
    let error = &failed["structuredContent"]["error"];
    assert_eq!(error["code"], "validation");
    assert_eq!(error["collection"], "lines");
    assert_eq!(error["line"], 2);
    assert!(
        error["message"].as_str().unwrap().starts_with("line 2:"),
        "{error}"
    );
    // The text carries the same, for a model that reads only text.
    let text: Value = serde_json::from_str(failed["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["error"], *error);

    // A refusal because of the state of a document.
    let draft = t.draft(&t.admin, &c, "100.00").await;
    let refused = call(
        &t,
        &t.admin,
        "journal_entry_reverse",
        json!({ "id": id(&draft) }),
    )
    .await;
    assert_eq!(refused["isError"], true);
    let error = &refused["structuredContent"]["error"];
    assert_eq!(
        (&error["code"], &error["reason"]),
        (&json!("conflict"), &json!("state"))
    );

    // An input that cannot be read names the field.
    let unread = call(&t, &t.admin, "journal_entry_post", json!({ "id": "nope" })).await;
    assert_eq!(unread["isError"], true);
    assert_eq!(unread["structuredContent"]["error"]["field"], "id");

    // What is wrong with the request itself, not with what it asks for,
    // is a protocol error and no tool result.
    for params in [
        json!({ "name": "no_such_tool", "arguments": {} }),
        json!({ "name": "meta_describe", "arguments": [] }),
    ] {
        let (status, body) = modern(&t, &t.admin, "tools/call", params).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602, "{body}");
        assert!(body.get("result").is_none());
    }
    let (_, body) = modern(
        &t,
        &t.admin,
        "tools/call",
        json!({ "name": "no_such_tool" }),
    )
    .await;
    assert_eq!(body["error"]["message"], "Unknown tool: no_such_tool");

    t.stop().await;
}

// ------------------------------------------------------------ discovery

#[tokio::test]
async fn an_agent_finds_the_tools_it_needs_without_reading_them_all() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // What there is.
    let modules =
        call(&t, &t.admin, "erpcraft_modules", json!({})).await["structuredContent"]["modules"]
            .clone();
    let module = |name: &str| {
        modules
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .unwrap_or_else(|| panic!("no module {name}"))
            .clone()
    };
    let purchasing = module("purchasing");
    assert_eq!(purchasing["label"], "Purchasing");
    // By name, to pass on, and label, to show.
    let documents: Vec<&str> = purchasing["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        documents,
        [
            "purchase_order",
            "purchase_receipt",
            "purchase_return",
            "vendor_credit",
            "vendor_credit_note",
            "vendor_invoice"
        ]
    );
    assert_eq!(purchasing["documents"][0]["label"], "Purchase order");
    assert!(
        purchasing["records"]
            .as_array()
            .unwrap()
            .contains(&json!({ "name": "vendor", "label": "Vendor" }))
    );
    assert!(
        purchasing["depends_on"]
            .as_array()
            .unwrap()
            .contains(&json!("gl"))
    );
    assert!(purchasing["tools"].as_u64().unwrap() >= 8);
    assert!(
        module("gl")["reports"]
            .as_array()
            .unwrap()
            .contains(&json!("Trial balance"))
    );
    // The counts add up to the commands there are.
    let counted: u64 = modules
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["tools"].as_u64().unwrap())
        .sum();
    assert_eq!(counted as usize, t.kernel.registry().commands().count());

    let found = |token: &str, arguments: Value| {
        let (t, token) = (&t, token.to_owned());
        async move {
            let result = call(t, &token, "erpcraft_tools", arguments).await;
            assert_eq!(result["isError"], false, "{result}");
            result["structuredContent"]["tools"]
                .as_array()
                .unwrap()
                .clone()
        }
    };

    // The tools of one module, complete enough to call.
    let tools = found(&t.admin, json!({ "module": "purchasing" })).await;
    assert_eq!(tools.len() as u64, purchasing["tools"].as_u64().unwrap());
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let meta = &t.kernel.registry().command_by_tool(name).unwrap().meta;
        assert_eq!(meta.module, "purchasing", "{name}");
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["title"].is_string() && tool["annotations"].is_object());
    }
    // The same definitions the list gives.
    let listed = all_tools(&t, &t.admin).await;
    assert_eq!(
        named(&tools, "purchase_order_approve"),
        named(&listed, "purchase_order_approve")
    );

    // The tools for one resource: those that act on it, and those that
    // make one.
    let on_orders = found(&t.admin, json!({ "resource": "purchase_order" })).await;
    for name in [
        "purchase_order_approve",
        "purchase_order_receive",
        "purchase_order_invoice",
    ] {
        assert!(names(&on_orders).contains(&name), "{name}");
    }
    let for_invoices = found(&t.admin, json!({ "resource": "vendor_invoice" })).await;
    assert_eq!(
        names(&for_invoices),
        [
            // The generic tools that read and write it come first.
            "document_create",
            "document_delete",
            "document_get",
            "document_list",
            "document_update",
            "einvoice_import",
            "intercompany_mirror",
            "purchase_order_invoice",
            "vendor_invoice_credit",
            "vendor_invoice_post",
            "vendor_invoice_reverse"
        ]
    );
    let both = found(
        &t.admin,
        json!({ "module": "sales", "resource": "purchase_order" }),
    )
    .await;
    assert!(both.is_empty());

    // What the caller can never run is left out here too.
    let clerk = clerk(&t, &c).await;
    assert!(
        found(&clerk, json!({ "module": "purchasing" }))
            .await
            .is_empty()
    );
    // For journal entries, the clerk may draft and read them: the
    // generic tools for that, and not posting.
    assert_eq!(
        names(&found(&clerk, json!({ "resource": "journal_entry" })).await),
        ["document_create", "document_get", "document_list"]
    );
    let kernel_tools = found(&clerk, json!({ "module": "kernel" })).await;
    assert!(names(&kernel_tools).contains(&"document_create"));
    assert!(!names(&kernel_tools).contains(&"user_create"));
    let seen = call(&t, &clerk, "erpcraft_modules", json!({})).await;
    let seen = seen["structuredContent"]["modules"]
        .as_array()
        .unwrap()
        .clone();
    let purchasing = seen.iter().find(|m| m["name"] == "purchasing").unwrap();
    assert_eq!(purchasing["tools"], 0);

    // Asked wrongly, it says how to ask.
    for (arguments, field, why) in [
        (
            json!({}),
            Value::Null,
            "give a module, a resource, or names",
        ),
        (
            json!({ "resource": "invoice" }),
            json!("resource"),
            "names no entity or document type",
        ),
        (
            json!({ "module": "buying" }),
            json!("module"),
            "erpcraft_modules lists the modules",
        ),
        (json!({ "module": 3 }), json!("module"), "must be a text"),
        (json!({ "modul": "gl" }), json!("modul"), "unknown field"),
    ] {
        let result = call(&t, &t.admin, "erpcraft_tools", arguments).await;
        assert_eq!(result["isError"], true, "{result}");
        let error = &result["structuredContent"]["error"];
        assert_eq!(error["code"], "validation");
        assert_eq!(error.get("field").cloned().unwrap_or(Value::Null), field);
        assert!(error["message"].as_str().unwrap().contains(why), "{error}");
    }

    t.stop().await;
}

// ------------------------------------------------------------ resources

#[tokio::test]
async fn reference_material_is_served_as_resources() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    let listed = result(&t, &t.admin, "resources/list", json!({})).await;
    assert_eq!(listed["cacheScope"], "public", "the same for every caller");
    let resources = listed["resources"].as_array().unwrap();
    let uris: Vec<&str> = resources
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    for uri in [
        "erpcraft://guide/commands",
        "erpcraft://guide/errors",
        "erpcraft://guide/documents",
        "erpcraft://guide/policies",
        "erpcraft://policies",
        "erpcraft://modules/gl",
        "erpcraft://modules/purchasing",
    ] {
        assert!(uris.contains(&uri), "{uri} is not listed");
    }
    for resource in resources {
        for field in ["name", "title", "description", "mimeType"] {
            assert!(!resource[field].as_str().unwrap().is_empty(), "{resource}");
        }
    }

    // Every resource that is listed can be read, by anyone signed in.
    let clerk = clerk(&t, &c).await;
    for resource in resources {
        let uri = resource["uri"].as_str().unwrap();
        for token in [&t.admin, &clerk] {
            let read = result(&t, token, "resources/read", json!({ "uri": uri })).await;
            assert_eq!(read["cacheScope"], "public");
            let contents = read["contents"].as_array().unwrap();
            assert_eq!(contents.len(), 1, "{uri}");
            assert_eq!(contents[0]["uri"], uri);
            assert_eq!(contents[0]["mimeType"], resource["mimeType"]);
            let text = contents[0]["text"].as_str().unwrap();
            assert!(text.len() > 200, "{uri} is nearly empty");
            if resource["mimeType"] == "application/json" {
                serde_json::from_str::<Value>(text).unwrap_or_else(|e| panic!("{uri}: {e}"));
            }
        }
    }
    let text = |uri: &'static str| {
        let t = &t;
        async move {
            let read = result(t, &t.admin, "resources/read", json!({ "uri": uri })).await;
            read["contents"][0]["text"].as_str().unwrap().to_owned()
        }
    };

    // The guides are the architecture document's own words.
    let documents = text("erpcraft://guide/documents").await;
    assert!(documents.starts_with("### Documents and postings"));
    assert!(documents.contains("A **posting** is the immutable record"));
    assert!(documents.contains("### Documents that follow from each other"));
    assert!(
        !documents.contains("### Ledgers own their rules"),
        "one section, not the rest"
    );
    assert!(
        text("erpcraft://guide/commands")
            .await
            .contains("an idempotency key")
    );
    assert!(text("erpcraft://guide/errors").await.contains("`reason`"));
    assert!(
        text("erpcraft://guide/policies")
            .await
            .contains("**A policy point**")
    );

    // The policy points, with the facts a rule may read.
    let policies: Value = serde_json::from_str(&text("erpcraft://policies").await).unwrap();
    let point = policies["policies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "purchase_receipt.over_receipt")
        .expect("the over-receipt point");
    let facts: Vec<&str> = point["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(facts.contains(&"over_percent"), "{facts:?}");
    assert_eq!(
        policies["policies"].as_array().unwrap().len(),
        t.kernel.registry().policies().count()
    );

    // One module, as the registry describes it.
    let purchasing: Value =
        serde_json::from_str(&text("erpcraft://modules/purchasing").await).unwrap();
    assert_eq!(purchasing["modules"][0]["name"], "purchasing");
    let doc_types: Vec<&str> = purchasing["doc_types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        doc_types,
        [
            "purchase_order",
            "purchase_receipt",
            "purchase_return",
            "vendor_credit",
            "vendor_credit_note",
            "vendor_invoice"
        ]
    );
    assert!(
        purchasing["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["module"] == "purchasing")
    );
    assert!(
        purchasing.get("access").is_none(),
        "nothing about the caller"
    );

    // The modules are also offered as a template.
    let templates = result(&t, &t.admin, "resources/templates/list", json!({})).await;
    assert_eq!(
        templates["resourceTemplates"][0]["uriTemplate"],
        "erpcraft://modules/{module}"
    );

    // What is not there is an error, never an empty answer. The code is
    // the one each revision defines.
    for uri in [
        "erpcraft://modules/nope",
        "erpcraft://guide/nope",
        "file:///etc/passwd",
        "",
    ] {
        let (status, body) = modern(&t, &t.admin, "resources/read", json!({ "uri": uri })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602, "{body}");
        assert_eq!(body["error"]["data"]["uri"], uri);
        let (_, body) = legacy(
            &t,
            &t.admin,
            Some("2025-11-25"),
            "resources/read",
            json!({ "uri": uri }),
        )
        .await;
        assert_eq!(body["error"]["code"], -32002, "{body}");
    }
    // Records are not resources: they are read with tools, which check
    // what this caller may see.
    assert!(uris.iter().all(|uri| {
        [
            "erpcraft://guide/",
            "erpcraft://policies",
            "erpcraft://modules/",
        ]
        .iter()
        .any(|kind| uri.starts_with(kind))
    }));

    t.stop().await;
}

// -------------------------------------------------------------- prompts

#[tokio::test]
async fn prompts_are_real_work_with_the_tools_that_exist() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "USD").await;

    // Every command a prompt names is registered. A prompt that names one
    // that is gone would not be offered; this makes that a failure.
    let named_commands = erpcraft_mcp::prompt_commands();
    assert_eq!(named_commands.len(), 3);
    for (prompt, commands) in &named_commands {
        assert!(commands.len() >= 3, "{prompt} is about tools");
        for command in commands {
            assert!(
                t.kernel.registry().command(command).is_ok(),
                "prompt {prompt} names `{command}`, which is not a registered command"
            );
        }
    }

    let listed = result(&t, &t.admin, "prompts/list", json!({})).await;
    let prompts = listed["prompts"].as_array().unwrap();
    let offered: Vec<&str> = prompts
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        offered,
        ["enter_vendor_invoice", "close_month", "explain_refusal"]
    );
    for prompt in prompts {
        assert!(!prompt["title"].as_str().unwrap().is_empty());
        assert!(prompt["description"].as_str().unwrap().len() > 40);
        for argument in prompt["arguments"].as_array().unwrap() {
            assert!(argument["name"].is_string() && argument["required"].is_boolean());
            assert!(!argument["description"].as_str().unwrap().is_empty());
        }
    }

    let get = |name: &'static str, arguments: Value| {
        let t = &t;
        async move {
            let got = result(
                t,
                &t.admin,
                "prompts/get",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
            let messages = got["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0]["role"], "user");
            assert_eq!(messages[0]["content"]["type"], "text");
            messages[0]["content"]["text"].as_str().unwrap().to_owned()
        }
    };
    let tools = all_tools(&t, &t.admin).await;
    let tool_names = names(&tools);

    for (name, arguments, says) in [
        (
            "enter_vendor_invoice",
            json!({ "purchase_order": "PO-2026-00012", "invoice": "INV-77, 10 widgets at 2.00" }),
            vec![
                "PO-2026-00012",
                "INV-77, 10 widgets at 2.00",
                "preview: true",
                "idempotency_key",
            ],
        ),
        (
            "close_month",
            json!({ "company": "ACME", "period": "2026" }),
            vec![
                "period 2026 of company ACME",
                "Total debit must equal total credit",
                "ask whether to close",
            ],
        ),
        (
            "explain_refusal",
            json!({ "error": "{\"code\":\"conflict\",\"reason\":\"state\"}", "tool": "journal_entry_post" }),
            vec![
                "{\"code\":\"conflict\",\"reason\":\"state\"}",
                "journal_entry_post",
                "Its arguments: (not given)",
            ],
        ),
    ] {
        let text = get(name, arguments).await;
        for said in says {
            assert!(text.contains(said), "{name} does not say: {said}\n{text}");
        }
        // Nothing is left unfilled, and every tool it names can be called.
        assert!(
            !text.contains("{tool:") && !text.contains("{arg:"),
            "{text}"
        );
        let (_, commands) = named_commands
            .iter()
            .find(|(prompt, _)| *prompt == name)
            .unwrap();
        for command in commands {
            let tool = command.replace('.', "_");
            assert!(text.contains(&tool), "{name} lost {tool}");
            assert!(tool_names.contains(&tool.as_str()), "{tool} is not a tool");
        }
    }

    // A prompt is asked for properly or not at all.
    for (params, why) in [
        (
            json!({ "name": "close_month", "arguments": { "company": "ACME" } }),
            "needs the argument 'period'",
        ),
        (
            json!({ "name": "close_month", "arguments": { "company": "ACME", "period": " " } }),
            "needs the argument 'period'",
        ),
        (
            json!({ "name": "close_month", "arguments": { "company": "ACME", "period": 2026 } }),
            "must be a text",
        ),
        (
            json!({ "name": "close_month", "arguments": { "company": "A", "period": "P", "year": "2026" } }),
            "has no argument 'year'",
        ),
        (
            json!({ "name": "close_year" }),
            "Unknown prompt: close_year",
        ),
        (json!({}), "needs the name of a prompt"),
    ] {
        let (status, body) = modern(&t, &t.admin, "prompts/get", params).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32602, "{body}");
        assert!(
            body["error"]["message"].as_str().unwrap().contains(why),
            "{body}"
        );
    }

    // Someone who could not do the work is not offered it. Explaining a
    // refusal needs only tools everyone has.
    let clerk = clerk(&t, &c).await;
    let listed = result(&t, &clerk, "prompts/list", json!({})).await;
    assert_eq!(listed["cacheScope"], "private");
    let offered: Vec<&str> = listed["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(offered, ["explain_refusal"]);
    let (_, body) = modern(
        &t,
        &clerk,
        "prompts/get",
        json!({ "name": "close_month", "arguments": { "company": "ACME", "period": "2026" } }),
    )
    .await;
    assert_eq!(body["error"]["code"], -32602, "{body}");

    // The earlier revisions have prompts too.
    let (status, body) = legacy(&t, &t.admin, Some("2025-06-18"), "prompts/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["prompts"].as_array().unwrap().len(), 3);
    assert!(body["result"].get("ttlMs").is_none());

    t.stop().await;
}

// ------------------------------------------------------------ transport

#[tokio::test]
async fn the_transport_is_what_the_specification_asks_of_an_http_server() {
    let t = TestApp::start().await;
    let ping = || json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let post = |headers: Vec<(&'static str, &'static str)>| {
        let (t, body) = (&t, ping());
        async move {
            t.send("POST", "/mcp", Some(&t.admin), Some(body), &headers)
                .await
        }
    };

    // A page of another site is refused before anything else, signed in
    // or not, with an error that has no id.
    for origin in [
        "https://evil.example",
        "null",
        "https://localhost.evil.example",
    ] {
        let (status, body) = post(vec![("origin", origin), ("host", "erp.example.com")]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}: {body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("origin")
        );
        assert!(body.get("id").is_none());
        let (status, _) = t
            .send("POST", "/mcp", None, Some(ping()), &[("origin", origin)])
            .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "before the sign-in is looked at"
        );
    }
    // The server's own pages, pages on this machine, and callers that are
    // not web pages are let in.
    for headers in [
        vec![
            ("origin", "https://erp.example.com"),
            ("host", "erp.example.com"),
        ],
        vec![
            ("origin", "http://localhost:5173"),
            ("host", "erp.example.com"),
        ],
        vec![
            ("origin", "http://127.0.0.1:8600"),
            ("host", "erp.example.com"),
        ],
        vec![("host", "erp.example.com")],
        vec![],
    ] {
        let (status, body) = post(headers.clone()).await;
        assert_eq!(status, StatusCode::OK, "{headers:?}: {body}");
    }

    // With a list of origins, exactly those.
    let strict = erpcraft_mcp::router_with(
        t.kernel.clone(),
        Config::with_origins(&["https://app.example.com"]),
    );
    let asked = |origin: Option<&'static str>| {
        let (strict, token, body) = (strict.clone(), t.admin.clone(), ping());
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "erp.example.com")
                .header("authorization", format!("Bearer {token}"));
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = strict
                .oneshot(request.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            response.status()
        }
    };
    assert_eq!(asked(Some("https://app.example.com")).await, StatusCode::OK);
    assert_eq!(asked(None).await, StatusCode::OK);
    assert_eq!(
        asked(Some("https://erp.example.com")).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        asked(Some("http://localhost:5173")).await,
        StatusCode::FORBIDDEN
    );

    // There is no stream to open and no session to end.
    for method in ["GET", "DELETE"] {
        let response = t
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/mcp")
                    .header("authorization", format!("Bearer {}", t.admin))
                    .header("accept", "text/event-stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(response.headers()["allow"], "POST");
    }

    // An answer is one JSON object, and no session is handed out.
    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("authorization", format!("Bearer {}", t.admin))
                .header("accept", "application/json, text/event-stream")
                .header("mcp-session-id", "left-over-from-an-older-client")
                .body(Body::from(
                    json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                            "params": { "protocolVersion": "2025-11-25" } })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert!(response.headers().get("mcp-session-id").is_none());
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");

    // A client that can take JSON is served, whatever else it lists; one
    // that cannot is told.
    for (accept, status) in [
        ("application/json, text/event-stream", StatusCode::OK),
        ("text/event-stream, application/json;q=0.9", StatusCode::OK),
        ("*/*", StatusCode::OK),
        ("text/event-stream", StatusCode::NOT_ACCEPTABLE),
        ("text/html", StatusCode::NOT_ACCEPTABLE),
    ] {
        let (got, body) = post(vec![("accept", accept)]).await;
        assert_eq!(got, status, "{accept}: {body}");
    }

    // One message a request: no batches, and nothing that is not JSON.
    let (status, body) = t
        .send(
            "POST",
            "/mcp",
            Some(&t.admin),
            Some(json!([ping(), ping()])),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], -32600);
    let response = t
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("authorization", format!("Bearer {}", t.admin))
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let (status, body) = t
        .send(
            "POST",
            "/mcp",
            Some(&t.admin),
            Some(json!({ "jsonrpc": "2.0", "id": 1 })),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], -32600, "{body}");

    // Without a token there is nothing, in either era.
    let (status, _) = t.send("POST", "/mcp", None, Some(ping()), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = modern(&t, "not-a-token", "server/discover", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    t.stop().await;
}

#[tokio::test]
async fn tools_are_listed_in_a_fixed_order_across_pages() {
    let t = TestApp::start().await;
    let tools = t.all_tools(&t.admin).await;
    let names = names(&tools);
    // The two tools for finding one's way first, then every command by
    // name, each once: a client paging through gets the same list each
    // time.
    let (first, commands) = names.split_at(2);
    assert!(
        first.iter().all(|n| n.starts_with("erpcraft_")),
        "{first:?}"
    );
    let mut sorted = commands.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(commands, sorted.as_slice());
    assert!(names.contains(&"journal_entry_post"));
    assert_eq!(names, self::names(&t.all_tools(&t.admin).await));
    t.stop().await;
}

/// From the MCP trial (tools/mcp-trial): what an agent needs to write a
/// record or document comes with erpcraft_tools, the core tools are named
/// by erpcraft_modules, tools can be fetched by name, and a tool that
/// creates a document says what comes next.
#[tokio::test]
async fn an_agent_learns_what_to_write_and_what_comes_next() {
    let t = TestApp::start().await;
    let _c = t.company("ACME", "USD").await;

    let modules = call(&t, &t.admin, "erpcraft_modules", json!({})).await;
    let core: Vec<&str> = modules["structuredContent"]["core_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(core[0], "search_records");
    assert!(core.contains(&"entity_create") && core.contains(&"document_create"));

    // A resource: the tools to write it, and its fields.
    let got = call(
        &t,
        &t.admin,
        "erpcraft_tools",
        json!({ "resource": "sales_order" }),
    )
    .await;
    let structured = &got["structuredContent"];
    let tools: Vec<&str> = structured["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools[0], "document_create", "{tools:?}");
    assert!(tools.contains(&"sales_order_approve"));
    let resource = &structured["resource"];
    assert_eq!(resource["kind"], "document");
    let customer = resource["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "customer_id")
        .unwrap();
    assert_eq!(customer["type"], "link to customer (id)");
    assert_eq!(customer["required"], true);
    let lines = &resource["collections"][0];
    assert_eq!(lines["name"], "lines");
    let qty = lines["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "qty")
        .unwrap();
    assert_eq!(qty["type"], "decimal string");
    assert!(
        resource["states"]
            .as_array()
            .unwrap()
            .contains(&json!("draft"))
    );

    let customer = call(
        &t,
        &t.admin,
        "erpcraft_tools",
        json!({ "resource": "customer" }),
    )
    .await;
    let customer = &customer["structuredContent"];
    assert_eq!(customer["resource"]["kind"], "entity");
    assert!(
        customer["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "entity_create")
    );

    // By name, whatever the module; a name that is no tool is said.
    let by_name = call(
        &t,
        &t.admin,
        "erpcraft_tools",
        json!({ "names": ["gl_trial_balance", "vendor_invoice_post", "no_such_tool"] }),
    )
    .await;
    let by_name = &by_name["structuredContent"];
    assert_eq!(by_name["tools"].as_array().unwrap().len(), 2);
    assert_eq!(by_name["unknown"], json!(["no_such_tool"]));

    // A tool that makes a document names the tools that take it on.
    let listed = all_tools(&t, &t.admin).await;
    let import = named(&listed, "einvoice_import");
    let description = import["description"].as_str().unwrap();
    assert!(
        description.contains("next: vendor_invoice_post"),
        "{description}"
    );

    // A document's output schema is the same short one for every tool.
    let schema =
        serde_json::to_string(&named(&listed, "sales_order_approve")["outputSchema"]).unwrap();
    assert!(schema.len() < 2_500, "{} bytes", schema.len());

    // A list filter that names no field says which fields there are.
    let failed = call(
        &t,
        &t.admin,
        "entity_list",
        json!({ "entity": "currency", "filters": { "company_id": "x" } }),
    )
    .await;
    let message = failed["structuredContent"]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(message.contains("belong to no company"), "{message}");
    assert!(
        message.contains("the fields of currency are id, code"),
        "{message}"
    );

    t.stop().await;
}

/// A tool that takes a file takes a larger request over MCP too; others
/// keep the default.
#[tokio::test]
async fn a_file_tool_takes_a_large_request_and_others_do_not() {
    let t = TestApp::start().await;
    let big = "X".repeat(3 * 1024 * 1024);
    let file = call(&t, &t.admin, "edi_receive", json!({ "content": big })).await;
    assert!(file.get("structuredContent").is_some(), "{file}");
    let (status, body) = modern(
        &t,
        &t.admin,
        "tools/call",
        json!({ "name": "search_records", "arguments": { "q": big } }),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    t.stop().await;
}
