//! HTTP API.
//!
//! Every route runs a registered command through the kernel. The
//! `/api/commands/{name}` route reaches all of them; the entity and
//! document routes are REST-shaped shortcuts for the generic ones.
//!
//! Signing in is not here: the module that authenticates users serves its
//! own routes.

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use erpcraft_kernel::command::{self, Options, View};
use erpcraft_kernel::{Actor, Error, Kernel};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The API with a JSON index at `/` and a JSON "not found" for every
/// other path.
pub fn router(kernel: Kernel) -> Router {
    routes(kernel).route("/", get(index)).fallback(not_found)
}

/// The API routes alone, for a server that answers `/` and unknown paths
/// itself, for example with a user interface. It should still answer
/// unknown paths under `/api` with [`not_found`].
pub fn routes(kernel: Kernel) -> Router {
    Router::new()
        .route(
            "/healthz",
            get(|| async { Json(json!({ "status": "ok" })) }),
        )
        .route("/api/auth/me", get(me))
        .route("/api/meta", get(meta))
        .route("/api/commands/{name}", post(run_command))
        .route(
            "/api/entities/{entity}",
            get(entity_list).post(entity_create),
        )
        .route(
            "/api/entities/{entity}/{id}",
            get(entity_get).patch(entity_update).delete(entity_delete),
        )
        .route(
            "/api/documents/{doc_type}",
            get(document_list).post(document_create),
        )
        .route(
            "/api/documents/{doc_type}/{id}",
            get(document_get)
                .patch(document_update)
                .delete(document_delete),
        )
        .with_state(kernel)
}

/// The answer to a path that is not a route.
pub async fn not_found() -> Response {
    ApiError(Error::not_found("route")).into_response()
}

/// Where to go from the root. Needs no token.
pub async fn index() -> Json<Value> {
    Json(json!({
        "name": "erpcraft",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "health": "GET /healthz",
            "login": "POST /api/auth/login {email, password}",
            "describe": "GET /api/meta",
            "run_command": "POST /api/commands/{name}",
            "entities": "/api/entities/{entity}",
            "documents": "/api/documents/{doc_type}",
            "mcp": "POST /mcp",
        },
        "note": "Every endpoint except health and login needs 'Authorization: Bearer <token>'.",
    }))
}

// ------------------------------------------------------------------- errors

pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        ApiError(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Forbidden(_) => StatusCode::FORBIDDEN,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Internal details go to the log, not to the caller.
        if let Error::Internal(detail) = &self.0 {
            tracing::error!(%detail, "internal error");
        }
        (status, Json(json!({ "error": self.0.to_json() }))).into_response()
    }
}

pub type ApiResult = Result<Response, ApiError>;

pub fn ok(value: Value) -> ApiResult {
    Ok(Json(value).into_response())
}

fn created(value: Value) -> ApiResult {
    Ok((StatusCode::CREATED, Json(value)).into_response())
}

// --------------------------------------------------------------- extraction

pub fn bearer(headers: &HeaderMap) -> Result<&str, Error> {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(Error::Unauthorized)
}

/// The authenticated caller.
struct Auth(Actor);

impl FromRequestParts<Kernel> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, kernel: &Kernel) -> Result<Self, ApiError> {
        let token = bearer(&parts.headers)?;
        Ok(Auth(kernel.authenticate(token).await?))
    }
}

/// Parse a JSON body, treating an empty body as `{}`.
pub fn parse_body(body: &Bytes) -> Result<Value, Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice(body)
        .map_err(|e| Error::validation(format!("the request body is not valid JSON: {e}")))
}

fn parse_object(body: &Bytes) -> Result<Map<String, Value>, Error> {
    match parse_body(body)? {
        Value::Object(map) => Ok(map),
        _ => Err(Error::validation("the request body must be a JSON object")),
    }
}

fn idempotency_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

type Fields = Map<String, Value>;

/// Split the parameters every list takes (`limit`, `offset`, `q`, `where`, `sort`)
/// from the rest of a query string. `sort` is a comma-separated list.
fn paging(query: Vec<(String, String)>) -> Result<(Fields, Fields), Error> {
    let (mut page, mut rest) = (Map::new(), Map::new());
    for (key, value) in query {
        match key.as_str() {
            "limit" | "offset" => {
                let number: i64 = value
                    .parse()
                    .map_err(|_| Error::validation(format!("'{key}' must be a whole number")))?;
                page.insert(key, json!(number));
            }
            "q" | "where" => {
                page.insert(key, json!(value));
            }
            "active_only" => {
                page.insert(key, json!(value == "true"));
            }
            "sort" => {
                let fields: Vec<&str> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|f| !f.is_empty())
                    .collect();
                page.insert(key, json!(fields));
            }
            _ => {
                rest.insert(key, json!(value));
            }
        }
    }
    Ok((page, rest))
}

async fn run(
    kernel: &Kernel,
    actor: &Actor,
    name: &str,
    input: Value,
    headers: &HeaderMap,
) -> Result<Value, Error> {
    let options = Options {
        idempotency_key: idempotency_key(headers),
        preview: false,
        view: View::Full,
    };
    command::execute(kernel, actor, name, input, options).await
}

// ----------------------------------------------------------------- handlers

async fn me(Auth(actor): Auth) -> ApiResult {
    ok(json!(actor))
}

#[derive(Deserialize)]
struct MetaQuery {
    module: Option<String>,
}

async fn meta(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Query(query): Query<MetaQuery>,
    headers: HeaderMap,
) -> ApiResult {
    ok(run(
        &kernel,
        &actor,
        "meta.describe",
        json!({ "module": query.module }),
        &headers,
    )
    .await?)
}

#[derive(Deserialize)]
struct CommandQuery {
    #[serde(default)]
    preview: bool,
    /// `header`: a command that returns a document returns its header and
    /// summary only, without body, postings and links.
    #[serde(default)]
    view: View,
}

async fn run_command(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path(name): Path<String>,
    Query(query): Query<CommandQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let options = Options {
        idempotency_key: idempotency_key(&headers),
        preview: query.preview,
        view: query.view,
    };
    let mut output = command::execute(&kernel, &actor, &name, parse_body(&body)?, options).await?;
    if query.view == View::Header
        && let Some(document) = output.as_object_mut()
        && document.contains_key("doc_type")
    {
        for left_out in ["body", "postings", "links"] {
            document.remove(left_out);
        }
    }
    if query.preview {
        ok(json!({ "preview": true, "result": output }))
    } else if erpcraft_kernel::approval::is_pending(&output) {
        // The command did not run: it waits for approval.
        Ok((StatusCode::ACCEPTED, Json(output)).into_response())
    } else {
        ok(output)
    }
}

async fn entity_list(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path(entity): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> ApiResult {
    let (mut input, filters) = paging(query)?;
    input.insert("entity".into(), json!(entity));
    input.insert("filters".into(), Value::Object(filters));
    ok(run(
        &kernel,
        &actor,
        "entity.list",
        Value::Object(input),
        &headers,
    )
    .await?)
}

async fn entity_create(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path(entity): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let input = json!({ "entity": entity, "data": parse_body(&body)? });
    created(run(&kernel, &actor, "entity.create", input, &headers).await?)
}

async fn entity_get(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path((entity, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    ok(run(
        &kernel,
        &actor,
        "entity.get",
        json!({ "entity": entity, "id": id }),
        &headers,
    )
    .await?)
}

async fn entity_update(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path((entity, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    // A record sent back as it was read carries its version token.
    let mut data = parse_object(&body)?;
    let version = data.remove("_version");
    let input = json!({ "entity": entity, "id": id, "data": data, "version": version });
    ok(run(&kernel, &actor, "entity.update", input, &headers).await?)
}

#[derive(Deserialize)]
struct VersionQuery {
    version: Option<String>,
}

async fn entity_delete(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path((entity, id)): Path<(String, String)>,
    Query(query): Query<VersionQuery>,
    headers: HeaderMap,
) -> ApiResult {
    let input = json!({ "entity": entity, "id": id, "version": query.version });
    ok(run(&kernel, &actor, "entity.delete", input, &headers).await?)
}

async fn document_list(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path(doc_type): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> ApiResult {
    let (mut input, filters) = paging(query)?;
    // `summary.vendor_id=...` filters by a field of the type's summary.
    let mut summary = Map::new();
    for (key, value) in filters {
        match key.strip_prefix("summary.") {
            Some(field) => summary.insert(field.to_owned(), value),
            None => input.insert(key, value),
        };
    }
    input.insert("summary".into(), Value::Object(summary));
    input.insert("doc_type".into(), json!(doc_type));
    ok(run(
        &kernel,
        &actor,
        "document.list",
        Value::Object(input),
        &headers,
    )
    .await?)
}

async fn document_create(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path(doc_type): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let mut input = parse_object(&body)?;
    input.insert("doc_type".into(), json!(doc_type));
    created(
        run(
            &kernel,
            &actor,
            "document.create",
            Value::Object(input),
            &headers,
        )
        .await?,
    )
}

async fn document_get(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path((doc_type, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    ok(run(
        &kernel,
        &actor,
        "document.get",
        json!({ "doc_type": doc_type, "id": id }),
        &headers,
    )
    .await?)
}

async fn document_update(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path((doc_type, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let mut input = parse_object(&body)?;
    input.insert("doc_type".into(), json!(doc_type));
    input.insert("id".into(), json!(id));
    ok(run(
        &kernel,
        &actor,
        "document.update",
        Value::Object(input),
        &headers,
    )
    .await?)
}

async fn document_delete(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path((doc_type, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    ok(run(
        &kernel,
        &actor,
        "document.delete",
        json!({ "doc_type": doc_type, "id": id }),
        &headers,
    )
    .await?)
}
