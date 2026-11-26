//! Model Context Protocol server.
//!
//! Every registered command is an MCP tool. A tool call runs the same
//! handler, with the same authorization, as the HTTP API. Besides tools the
//! server offers reference material as resources and a few pieces of real
//! work as prompts. The transport is JSON-RPC over `POST /mcp`, one message
//! a request, authenticated with the same bearer token as the HTTP API.
//! Nothing is kept between requests.
//!
//! # Protocol revisions
//!
//! The server is built to revision **2026-07-28** of the specification
//! (<https://modelcontextprotocol.io/specification/2026-07-28>), which made
//! the protocol stateless: there is no `initialize` handshake, every request
//! says which revision it speaks in `params._meta`, and `server/discover`
//! tells a client what the server supports. The server also answers clients
//! of the revisions before that ("legacy": 2025-11-25, 2025-06-18,
//! 2025-03-26, 2024-11-05), which open with `initialize`. The specification
//! calls such a server dual-era, and it tells the two apart the way the
//! specification says: a request carrying the per-request `_meta` is served
//! by the current revision, `initialize` selects a legacy one.
//!
//! What each revision added, and so what a client of an older one is not
//! sent:
//!
//! | Revision   | Added (of what this server uses)                              |
//! |------------|---------------------------------------------------------------|
//! | 2024-11-05 | tools, resources, prompts, pagination                         |
//! | 2025-03-26 | tool annotations; the Streamable HTTP transport               |
//! | 2025-06-18 | `title`, `outputSchema`, `structuredContent`; the version header |
//! | 2025-11-25 | `403` for a refused `Origin`                                  |
//! | 2026-07-28 | per-request `_meta`, `server/discover`, `resultType`, `ttlMs` and `cacheScope`, the `Mcp-Method` and `Mcp-Name` headers; no `ping`, no sessions, no GET |
//!
//! # What is not built, and why
//!
//! - No server-sent streams, `subscriptions/listen`, or list-changed
//!   notifications: the set of tools changes only when the server is
//!   rebuilt or a role is changed, and the `ttlMs` on every list says how
//!   long to trust it. The capabilities say so.
//! - No sessions: the current revision has none, and the legacy ones make
//!   them optional.
//! - No elicitation, sampling, roots, logging, completion or tasks: nothing
//!   here needs to ask the client for anything.
//! - Records and documents are not resources. Reading one needs the
//!   caller's row and field permissions, filters and paging, which is what
//!   the `entity_*` and `document_*` tools do. Resources hold only what is
//!   the same for every caller.

mod discover;
mod prompts;
mod resources;
mod tools;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ACCEPT, AUTHORIZATION, HOST, ORIGIN, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use erpcraft_kernel::{Actor, Error, Kernel, command};
use serde_json::{Value, json};

pub use prompts::prompt_commands;

/// The revision this server is built to.
pub const CURRENT_VERSION: &str = "2026-07-28";

/// Revisions served statelessly, by per-request metadata.
const MODERN_VERSIONS: &[&str] = &[CURRENT_VERSION];

/// Revisions served after an `initialize` handshake, newest first.
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What a legacy request without a version header is taken to speak, as
/// the specification says.
const ASSUMED_LEGACY_VERSION: &str = "2025-03-26";

/// What a JSON-RPC request adds around a tool's input: the envelope, the
/// tool's name and `_meta`.
const RPC_OVERHEAD_BYTES: usize = 64 * 1024;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

// JSON-RPC and MCP error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_VERSION: i64 = -32022;
/// "Resource not found" up to 2025-11-25; the current revision uses
/// [`INVALID_PARAMS`].
const LEGACY_RESOURCE_NOT_FOUND: i64 = -32002;

/// How long a client may trust a list before asking again. The lists
/// change only when the server is rebuilt or a role is changed.
const LIST_TTL_MS: u64 = 5 * 60 * 1000;

const INSTRUCTIONS: &str = "erpcraft is an ERP. Every tool is a business command and runs with the \
permissions of the signed-in user, capped by the scopes of the token; a tool the user can never run \
is not listed.\n\
Finding your way: call erpcraft_modules to see the modules, their records and document types by \
name, and the core tools that work on any of them (search_records finds anything by a code, name or \
number). Then call erpcraft_tools with a resource (an entity or document type such as customer or \
sales_order) to get the tools for it and the fields to write; with a module for all its tools; or \
with `names` for tools you already know of. The resources under erpcraft://guide explain how \
commands, documents, postings and reversals work.\n\
Changing data: money and quantities are decimal strings, never numbers. Pass `preview: true` first \
on anything that posts, reverses, cancels or deletes: it returns what would happen and saves \
nothing. Pass an `idempotency_key` on every write, so that a retry after a timeout cannot apply the \
change twice.\n\
Errors: a failed tool returns isError with structuredContent.error: `code`, a `message` that is a \
complete sentence, and where known the `field`, the `collection` and `line` of a document body, and \
for a conflict the `reason` (stale, state, duplicate, in_use). A refusal for lack of permission or \
by a company policy is final for you: report it, do not work around it.";

/// What an installation can set.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The web origins whose pages may call the endpoint, such as
    /// `https://erp.example.com`. `None` is the default: a page served by
    /// this server itself, and pages on this machine (`localhost`,
    /// `127.0.0.1`, `[::1]`). With a list, exactly the origins in it.
    ///
    /// A request without an `Origin` header does not come from a web page
    /// and is always let in: an agent is such a caller.
    pub allowed_origins: Option<Vec<String>>,
}

impl Config {
    /// From the environment: `ERPCRAFT_MCP_ALLOWED_ORIGINS`, a list of
    /// origins separated by commas. Unset or empty leaves the default.
    pub fn from_env() -> Config {
        let allowed_origins = std::env::var("ERPCRAFT_MCP_ALLOWED_ORIGINS")
            .ok()
            .map(|list| {
                list.split(',')
                    .map(|origin| origin.trim().trim_end_matches('/').to_ascii_lowercase())
                    .filter(|origin| !origin.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|list| !list.is_empty());
        Config { allowed_origins }
    }

    /// Only these origins.
    pub fn with_origins(origins: &[&str]) -> Config {
        Config {
            allowed_origins: Some(
                origins
                    .iter()
                    .map(|origin| origin.trim_end_matches('/').to_ascii_lowercase())
                    .collect(),
            ),
        }
    }
}

struct Server {
    kernel: Kernel,
    config: Config,
}

/// The MCP endpoint, configured from the environment.
pub fn router(kernel: Kernel) -> Router {
    router_with(kernel, Config::from_env())
}

/// The MCP endpoint. Only POST is routed, so GET and DELETE, which earlier
/// revisions used for streams and sessions, are answered with `405 Method
/// Not Allowed`, as the specification asks of a server that has neither.
pub fn router_with(kernel: Kernel, config: Config) -> Router {
    // Each tool's own limit is checked in `handle`; the route takes the
    // largest any tool declares.
    let largest = kernel
        .registry()
        .commands()
        .map(|c| c.meta.max_input_bytes)
        .max()
        .unwrap_or(command::DEFAULT_MAX_INPUT_BYTES)
        + RPC_OVERHEAD_BYTES;
    Router::new()
        .route(
            "/mcp",
            post(handle).layer(axum::extract::DefaultBodyLimit::max(largest)),
        )
        .with_state(Arc::new(Server { kernel, config }))
}

/// Which revision a request is served by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Speaking {
    version: &'static str,
    /// Stateless, by per-request metadata: revision 2026-07-28 and later.
    modern: bool,
}

impl Speaking {
    /// Whether the revision has what `since` introduced. Revisions are
    /// dates, so they order as text.
    fn has(&self, since: &str) -> bool {
        self.version >= since
    }
}

/// A request as the handlers of one method see it.
struct Request<'a> {
    kernel: &'a Kernel,
    actor: &'a Actor,
    speaking: Speaking,
    params: &'a Value,
}

/// What a handler answers: a result, or a JSON-RPC error.
type Answer = std::result::Result<Value, RpcError>;

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn invalid(message: impl Into<String>) -> RpcError {
        RpcError::new(INVALID_PARAMS, message)
    }

    fn with_data(mut self, data: Value) -> RpcError {
        self.data = Some(data);
        self
    }

    fn body(&self, id: Option<&Value>) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        let mut body = json!({ "jsonrpc": "2.0", "error": error });
        if let Some(id) = id {
            body["id"] = id.clone();
        }
        body
    }

    fn respond(&self, status: StatusCode, id: Option<&Value>) -> Response {
        (status, Json(self.body(id))).into_response()
    }
}

fn server_info() -> Value {
    json!({ "name": "erpcraft", "title": "erpcraft", "version": env!("CARGO_PKG_VERSION") })
}

/// What the server offers. No notifications are ever sent, and the
/// capabilities say so.
fn capabilities() -> Value {
    json!({
        "tools": { "listChanged": false },
        "resources": { "subscribe": false, "listChanged": false },
        "prompts": { "listChanged": false },
    })
}

fn supported_versions() -> Vec<&'static str> {
    MODERN_VERSIONS
        .iter()
        .chain(LEGACY_VERSIONS)
        .copied()
        .collect()
}

/// Is a page of this origin let in? See [`Config::allowed_origins`].
///
/// "Served by this server itself" is judged by the `Host` header. That is
/// enough to keep other sites' pages out, and it is all that can be known
/// without configuration; it does not stop DNS rebinding, where the
/// attacker's name is both origin and host. What stops that here is that
/// the endpoint takes a bearer token and no cookie, which such a page does
/// not have. An installation reachable from browsers should set the list.
fn origin_allowed(config: &Config, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let origin = origin.trim().trim_end_matches('/').to_ascii_lowercase();
    if let Some(allowed) = &config.allowed_origins {
        return allowed.contains(&origin);
    }
    let Some((scheme, authority)) = origin.split_once("://") else {
        // `null`, the origin of a sandboxed page or a file.
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let host = match authority.strip_prefix('[') {
        // An IPv6 address in brackets, with or without a port.
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return true;
    }
    headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .is_some_and(|host| host.trim().eq_ignore_ascii_case(authority))
}

/// The value of a header that must be text.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A header value that may be in the specification's Base64 form,
/// `=?base64?...?=`, decoded. `None` when it claims the form and is not.
fn decoded(value: &str) -> Option<String> {
    let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|rest| rest.strip_suffix("?="))
    else {
        return Some(value.to_owned());
    };
    let mut bytes = Vec::with_capacity(encoded.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u8);
    for c in encoded.bytes().filter(|c| *c != b'=') {
        let six = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(six);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    String::from_utf8(bytes).ok()
}

async fn handle(State(server): State<Arc<Server>>, headers: HeaderMap, body: Bytes) -> Response {
    // Before anything else, and whatever the request is: a page of another
    // site gets no answer it can use.
    if !origin_allowed(&server.config, &headers) {
        return RpcError::new(INVALID_REQUEST, "this origin may not call the MCP endpoint")
            .respond(StatusCode::FORBIDDEN, None);
    }
    // Every answer is one JSON object. A client must accept that; one
    // that says it accepts only something else cannot be served.
    if let Some(accept) = header(&headers, ACCEPT.as_str()) {
        let accepts_json = accept.split(',').any(|kind| {
            let kind = kind.split(';').next().unwrap_or_default().trim();
            matches!(kind, "application/json" | "application/*" | "*/*")
        });
        if !accepts_json {
            return RpcError::new(
                INVALID_REQUEST,
                "the MCP endpoint answers with application/json; the Accept header must allow it",
            )
            .respond(StatusCode::NOT_ACCEPTABLE, None);
        }
    }

    let token = header(&headers, AUTHORIZATION.as_str())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let actor = match token {
        Some(token) => server.kernel.authenticate(token).await,
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
        return RpcError::new(PARSE_ERROR, "the request is not valid JSON")
            .respond(StatusCode::BAD_REQUEST, None);
    };
    if message.is_array() {
        // Batches existed in 2025-03-26 only.
        return RpcError::new(
            INVALID_REQUEST,
            "send one JSON-RPC message per request; batches are not supported",
        )
        .respond(StatusCode::BAD_REQUEST, None);
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return RpcError::new(INVALID_REQUEST, "the request has no method")
            .respond(StatusCode::BAD_REQUEST, None);
    };
    // A message without an id is a notification and gets no reply. The
    // only one a client sends is `notifications/initialized`, to which a
    // server that keeps nothing between requests has nothing to do.
    let Some(id) = message.get("id").filter(|id| !id.is_null()).cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    // A tool that takes a file takes a larger request, as over HTTP.
    let limit = match method {
        "tools/call" => params
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| server.kernel.registry().command_by_tool(name))
            .map_or(command::DEFAULT_MAX_INPUT_BYTES, |c| c.meta.max_input_bytes),
        _ => command::DEFAULT_MAX_INPUT_BYTES,
    } + RPC_OVERHEAD_BYTES;
    if body.len() > limit {
        return RpcError::new(
            INVALID_REQUEST,
            format!(
                "the request is {} bytes; at most {limit} are taken",
                body.len()
            ),
        )
        .respond(StatusCode::PAYLOAD_TOO_LARGE, Some(&id));
    }

    let header_version = header(&headers, "mcp-protocol-version").map(str::trim);
    let body_version = params
        .get("_meta")
        .and_then(|meta| meta.get(META_VERSION))
        .and_then(Value::as_str);
    let modern = method != "initialize"
        && (body_version.is_some() || header_version.is_some_and(|v| MODERN_VERSIONS.contains(&v)));

    let unsupported = |requested: &str| {
        RpcError::new(UNSUPPORTED_VERSION, "Unsupported protocol version")
            .with_data(json!({ "supported": supported_versions(), "requested": requested }))
            .respond(StatusCode::BAD_REQUEST, Some(&id))
    };

    let speaking = if modern {
        // Everything the current revision requires of a request, in the
        // order a client can fix it.
        let Some(requested) = body_version else {
            return RpcError::invalid(format!(
                "the request must say which revision it speaks in params._meta[\"{META_VERSION}\"]"
            ))
            .respond(StatusCode::BAD_REQUEST, Some(&id));
        };
        let Some(version) = MODERN_VERSIONS.iter().find(|v| **v == requested) else {
            return unsupported(requested);
        };
        let mismatch = |what: String| {
            RpcError::new(HEADER_MISMATCH, format!("Header mismatch: {what}"))
                .respond(StatusCode::BAD_REQUEST, Some(&id))
        };
        match header_version {
            Some(sent) if sent == requested => {}
            Some(sent) => {
                return mismatch(format!(
                    "MCP-Protocol-Version header value '{sent}' does not match body value '{requested}'"
                ));
            }
            None => return mismatch("the MCP-Protocol-Version header is missing".into()),
        }
        match header(&headers, "mcp-method").map(str::trim) {
            Some(sent) if sent == method => {}
            Some(sent) => {
                return mismatch(format!(
                    "Mcp-Method header value '{sent}' does not match body value '{method}'"
                ));
            }
            None => return mismatch("the Mcp-Method header is missing".into()),
        }
        let named = match method {
            "tools/call" | "prompts/get" => params.get("name").and_then(Value::as_str),
            "resources/read" => params.get("uri").and_then(Value::as_str),
            _ => None,
        };
        if let Some(named) = named {
            match header(&headers, "mcp-name").map(str::trim).map(decoded) {
                Some(Some(sent)) if sent == named => {}
                Some(Some(sent)) => {
                    return mismatch(format!(
                        "Mcp-Name header value '{sent}' does not match body value '{named}'"
                    ));
                }
                Some(None) => return mismatch("the Mcp-Name header is not valid Base64".into()),
                None => return mismatch("the Mcp-Name header is missing".into()),
            }
        }
        let capabilities = params.get("_meta").and_then(|m| m.get(META_CAPABILITIES));
        if !capabilities.is_some_and(Value::is_object) {
            return RpcError::invalid(format!(
                "the request must state the client's capabilities in params._meta[\"{META_CAPABILITIES}\"], \
                 as an object"
            ))
            .respond(StatusCode::BAD_REQUEST, Some(&id));
        }
        Speaking {
            version,
            modern: true,
        }
    } else {
        let version = match header_version {
            None => ASSUMED_LEGACY_VERSION,
            Some(sent) => match LEGACY_VERSIONS.iter().find(|v| **v == sent) {
                Some(version) => version,
                None => return unsupported(sent),
            },
        };
        Speaking {
            version,
            modern: false,
        }
    };

    let request = Request {
        kernel: &server.kernel,
        actor: &actor,
        speaking,
        params: &params,
    };
    let answer = match (method, speaking.modern) {
        ("initialize", false) => initialize(&params),
        ("ping", false) => Ok(json!({})),
        ("server/discover", true) => Ok(json!({
            "supportedVersions": supported_versions(),
            "capabilities": capabilities(),
            "instructions": INSTRUCTIONS,
            "ttlMs": LIST_TTL_MS,
            "cacheScope": "public",
        })),
        ("tools/list", _) => tools::list(&request),
        ("tools/call", _) => tools::call(&request).await,
        ("resources/list", _) => resources::list(&request),
        ("resources/templates/list", _) => resources::templates(&request),
        ("resources/read", _) => resources::read(&request),
        ("prompts/list", _) => prompts::list(&request),
        ("prompts/get", _) => prompts::get(&request),
        _ => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )),
    };

    match answer {
        Ok(mut result) => {
            if speaking.modern {
                // What every result of the current revision carries.
                result["resultType"] = json!("complete");
                result["_meta"][META_SERVER] = server_info();
            }
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
        }
        Err(error) => {
            if error.code == INTERNAL_ERROR {
                tracing::error!(method, message = %error.message, "MCP request failed");
            }
            // The current revision tells a missing method from a missing
            // endpoint by the status; the legacy ones answer 200.
            let status = match (speaking.modern, error.code) {
                (true, METHOD_NOT_FOUND) => StatusCode::NOT_FOUND,
                _ => StatusCode::OK,
            };
            error.respond(status, Some(&id))
        }
    }
}

/// The handshake of the legacy revisions. The client's revision is taken
/// if the server has it; otherwise the newest legacy one is offered, and
/// the client disconnects if it cannot speak that.
fn initialize(params: &Value) -> Answer {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = LEGACY_VERSIONS
        .iter()
        .find(|v| **v == requested)
        .unwrap_or(&LEGACY_VERSIONS[0]);
    Ok(json!({
        "protocolVersion": version,
        "capabilities": capabilities(),
        "serverInfo": server_info(),
        "instructions": INSTRUCTIONS,
    }))
}

/// One page of a list, by the specification's cursor. The cursor is what
/// the last item of the page before has under `key`, its name or its
/// address: it stays valid while that item exists, whatever is added
/// around it.
fn page<'a>(
    items: &'a [Value],
    key: &str,
    params: &Value,
    size: usize,
) -> std::result::Result<(&'a [Value], Option<String>), RpcError> {
    let name = |item: &'a Value| item[key].as_str().unwrap_or_default();
    let start = match params.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(Value::String(cursor)) => items
            .iter()
            .position(|item| name(item) == cursor)
            .map(|at| at + 1)
            .ok_or_else(|| RpcError::invalid("the cursor is not one this server gave out"))?,
        Some(_) => return Err(RpcError::invalid("the cursor must be a string")),
    };
    let end = (start + size).min(items.len());
    let next = (end < items.len()).then(|| name(&items[end - 1]).to_owned());
    Ok((&items[start..end], next))
}

/// A list result: the page, where it goes on, and for the current revision
/// how long it may be kept and by whom.
fn listed(
    request: &Request<'_>,
    key: &str,
    items: Vec<Value>,
    next: Option<String>,
    scope: &str,
) -> Value {
    let mut result = json!({ key: items });
    if let Some(next) = next {
        result["nextCursor"] = json!(next);
    }
    if request.speaking.modern {
        result["ttlMs"] = json!(LIST_TTL_MS);
        result["cacheScope"] = json!(scope);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_str(host).unwrap());
        if let Some(origin) = origin {
            headers.insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
        }
        headers
    }

    #[test]
    fn origins_are_judged_by_the_default_and_by_the_list() {
        let default = Config::default();
        for (origin, host, allowed) in [
            // Not a web page.
            (None, "erp.example.com", true),
            // A page this server served.
            (Some("https://erp.example.com"), "erp.example.com", true),
            (Some("https://ERP.example.com/"), "erp.example.com", true),
            (
                Some("http://erp.example.com:8600"),
                "erp.example.com:8600",
                true,
            ),
            // A page on this machine.
            (Some("http://localhost:5173"), "erp.example.com", true),
            (Some("http://127.0.0.1:3000"), "erp.example.com", true),
            (Some("http://[::1]:3000"), "erp.example.com", true),
            // Anyone else.
            (Some("https://evil.example"), "erp.example.com", false),
            (
                Some("https://erp.example.com:444"),
                "erp.example.com",
                false,
            ),
            (
                Some("https://localhost.evil.example"),
                "erp.example.com",
                false,
            ),
            (Some("null"), "erp.example.com", false),
            (Some("file://"), "erp.example.com", false),
        ] {
            assert_eq!(
                origin_allowed(&default, &headers(origin, host)),
                allowed,
                "{origin:?} at {host}"
            );
        }
        // With a list, exactly the list: not even the server's own pages.
        let listed = Config::with_origins(&["https://app.example.com/"]);
        assert!(origin_allowed(&listed, &headers(None, "erp.example.com")));
        assert!(origin_allowed(
            &listed,
            &headers(Some("https://app.example.com"), "erp.example.com")
        ));
        for origin in ["https://erp.example.com", "http://localhost:5173"] {
            assert!(!origin_allowed(
                &listed,
                &headers(Some(origin), "erp.example.com")
            ));
        }
    }

    #[test]
    fn header_values_are_decoded() {
        assert_eq!(
            decoded("journal_entry_post").as_deref(),
            Some("journal_entry_post")
        );
        // The specification's own examples.
        assert_eq!(
            decoded("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, 世界")
        );
        assert_eq!(
            decoded("=?base64?IHBhZGRlZCA=?=").as_deref(),
            Some(" padded ")
        );
        assert_eq!(
            decoded("=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?=").as_deref(),
            Some("=?base64?literal?=")
        );
        assert_eq!(decoded("=?base64?not base64!?="), None);
    }

    #[test]
    fn a_list_is_paged_by_the_name_of_the_last_item() {
        let items: Vec<Value> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| json!({ "name": name }))
            .collect();
        let paged = |params: Value, size: usize| {
            page(&items, "name", &params, size)
                .ok()
                .map(|(page, next)| (page.len(), page.first().cloned(), next))
        };
        assert_eq!(
            paged(json!({}), 2),
            Some((2, Some(json!({ "name": "a" })), Some("b".into())))
        );
        assert_eq!(
            paged(json!({ "cursor": "b" }), 2),
            Some((2, Some(json!({ "name": "c" })), Some("d".into())))
        );
        assert_eq!(
            paged(json!({ "cursor": "d" }), 2),
            Some((1, Some(json!({ "name": "e" })), None))
        );
        // Exactly full: no cursor to an empty page.
        assert_eq!(paged(json!({}), 5).unwrap().2, None);
        assert_eq!(paged(json!({ "cursor": "x" }), 2), None);
        assert_eq!(paged(json!({ "cursor": 3 }), 2), None);
    }

    #[test]
    fn revisions_order_as_dates() {
        let speaking = |version| Speaking {
            version,
            modern: false,
        };
        assert!(speaking("2025-06-18").has("2025-06-18"));
        assert!(speaking("2025-11-25").has("2025-06-18"));
        assert!(!speaking("2025-03-26").has("2025-06-18"));
        assert!(speaking(CURRENT_VERSION).has("2025-11-25"));
    }
}
