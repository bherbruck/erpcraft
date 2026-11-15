//! Model Context Protocol server.
//!
//! Every registered command is an MCP tool. A tool call runs the same
//! handler, with the same authorization, as the HTTP API. The transport is
//! stateless JSON-RPC over `POST /mcp`, authenticated with the same bearer
//! token as the HTTP API.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use erpcraft_kernel::command::{self, CommandMeta, Options};
use erpcraft_kernel::{Actor, Error, Kernel};
use serde_json::{Value, json};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "erpcraft is an ERP. Each tool is a business command and runs with the \
permissions of the signed-in user. Call meta_describe first to learn the entities, document types and \
commands of a module. Money amounts are decimal strings. Tools that change data accept `preview: true` \
to return the outcome without saving it, and `idempotency_key` so a retry cannot apply a change twice.";

pub fn router(kernel: Kernel) -> Router {
    Router::new().route("/mcp", post(handle)).with_state(kernel)
}

fn rpc_result(id: Value, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
        .into_response()
}

async fn handle(State(kernel): State<Kernel>, headers: HeaderMap, body: Bytes) -> Response {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let actor = match token {
        Some(token) => kernel.authenticate(token).await,
        None => Err(Error::Unauthorized),
    };
    let actor = match actor {
        Ok(actor) => actor,
        Err(Error::Unauthorized) => {
            return (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, "Bearer")]).into_response();
        }
        Err(err) => {
            tracing::error!(%err, "authentication failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "the request is not valid JSON");
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return rpc_error(Value::Null, -32600, "the request has no method");
    };
    // A message without an id is a notification and gets no reply.
    let Some(id) = message.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .unwrap_or(&PROTOCOL_VERSIONS[0]);
            rpc_result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "erpcraft", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tools(&kernel, &actor) })),
        "tools/call" => call(&kernel, &actor, id, params).await,
        _ => rpc_error(id, -32601, "unknown method"),
    }
}

/// The tools the caller could use: commands whose permission the caller
/// holds for at least one company.
fn tools(kernel: &Kernel, actor: &Actor) -> Vec<Value> {
    kernel
        .registry()
        .commands()
        .map(|entry| &entry.meta)
        .filter(|meta| meta.permission.as_deref().is_none_or(|p| actor.can_any(p)))
        .map(tool)
        .collect()
}

fn tool(meta: &CommandMeta) -> Value {
    let mut schema = meta.input_schema.clone();
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
        object.entry("type").or_insert(json!("object"));
        if !meta.read_only {
            let properties = object.entry("properties").or_insert_with(|| json!({}));
            if let Some(properties) = properties.as_object_mut() {
                properties.insert(
                    "preview".into(),
                    json!({
                        "type": "boolean",
                        "description": "Return what the command would do without saving anything. \
                                        The real call validates again.",
                    }),
                );
                properties.insert(
                    "idempotency_key".into(),
                    json!({
                        "type": "string",
                        "description": "A unique value for this change. Repeating the call with the \
                                        same key and input returns the first result.",
                    }),
                );
            }
        }
    }
    json!({
        "name": meta.tool_name(),
        "description": meta.description,
        "inputSchema": schema,
        "annotations": { "readOnlyHint": meta.read_only },
    })
}

async fn call(kernel: &Kernel, actor: &Actor, id: Value, params: Value) -> Response {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(entry) = kernel.registry().command_by_tool(name) else {
        return rpc_error(id, -32602, &format!("unknown tool '{name}'"));
    };
    let mut arguments = match params.get("arguments").cloned() {
        None | Some(Value::Null) => json!({}),
        Some(value @ Value::Object(_)) => value,
        Some(_) => return rpc_error(id, -32602, "arguments must be an object"),
    };

    let mut options = Options::default();
    if !entry.meta.read_only {
        if let Some(object) = arguments.as_object_mut() {
            options.preview = object
                .remove("preview")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            options.idempotency_key = object
                .remove("idempotency_key")
                .and_then(|v| v.as_str().map(str::to_owned))
                .filter(|k| !k.is_empty());
        }
    }
    let preview = options.preview;

    let result = match command::execute(kernel, actor, &entry.meta.name, arguments, options).await {
        Ok(output) => {
            let structured = match output {
                _ if preview => json!({ "preview": true, "result": output }),
                Value::Object(_) => output,
                other => json!({ "result": other }),
            };
            json!({
                "content": [{ "type": "text", "text": structured.to_string() }],
                "structuredContent": structured,
                "isError": false,
            })
        }
        Err(err) => {
            let message = match &err {
                Error::Internal(detail) => {
                    tracing::error!(%detail, tool = name, "internal error");
                    "internal error".to_owned()
                }
                other => other.to_string(),
            };
            json!({
                "content": [{ "type": "text", "text": message }],
                "structuredContent": { "error": { "code": err.code(), "message": message } },
                "isError": true,
            })
        }
    };
    rpc_result(id, result)
}
