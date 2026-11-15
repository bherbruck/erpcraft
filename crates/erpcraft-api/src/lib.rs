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
use erpcraft_kernel::command::{self, Options};
use erpcraft_kernel::{Actor, Error, Kernel};
use serde::Deserialize;
use serde_json::{Map, Value, json};

pub fn router(kernel: Kernel) -> Router {
    Router::new()
        .route("/", get(index))
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
            get(entity_get).patch(entity_update),
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
        .fallback(|| async { ApiError(Error::not_found("route")) })
        .with_state(kernel)
}

/// Where to go from the root. Needs no token.
async fn index() -> Json<Value> {
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
        let message = match &self.0 {
            Error::Internal(detail) => {
                tracing::error!(%detail, "internal error");
                "internal error".to_owned()
            }
            other => other.to_string(),
        };
        (
            status,
            Json(json!({ "error": { "code": self.0.code(), "message": message } })),
        )
            .into_response()
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

/// Split paging parameters from the rest of a query string.
fn paging(query: Vec<(String, String)>) -> Result<(Fields, Fields), Error> {
    let (mut page, mut rest) = (Map::new(), Map::new());
    for (key, value) in query {
        if key == "limit" || key == "offset" {
            let number: i64 = value
                .parse()
                .map_err(|_| Error::validation(format!("'{key}' must be a whole number")))?;
            page.insert(key, json!(number));
        } else {
            rest.insert(key, json!(value));
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
    };
    let output = command::execute(&kernel, &actor, &name, parse_body(&body)?, options).await?;
    if query.preview {
        ok(json!({ "preview": true, "result": output }))
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
    let input = json!({ "entity": entity, "id": id, "data": parse_body(&body)? });
    ok(run(&kernel, &actor, "entity.update", input, &headers).await?)
}

async fn document_list(
    State(kernel): State<Kernel>,
    Auth(actor): Auth,
    Path(doc_type): Path<String>,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> ApiResult {
    let (mut input, filters) = paging(query)?;
    input.extend(filters);
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
