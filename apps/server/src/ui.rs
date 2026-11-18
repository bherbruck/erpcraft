//! Serves the built admin UI (the static files of `ui/dist`) next to the
//! API, so there is no separate UI server to deploy.
//!
//! Every path that is not a route and not under `/api` or `/mcp` returns
//! the file of that name, or else `index.html`: the UI is a single-page
//! app and does its own routing.

use std::convert::Infallible;
use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

/// The UI loads its script and stylesheet from this server and talks only
/// to this server. Inline styles are allowed because the dialog component
/// (Radix, through react-remove-scroll) adds a `<style>` element while a
/// dialog is open; inline scripts are not.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";
/// Always ask the server, so a new build is picked up at once.
const REVALIDATE: &str = "no-cache";
/// Vite names the files under `assets/` after their content.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[derive(Clone)]
struct Ui {
    files: ServeDir,
    document: ServeFile,
}

/// Answer every path `router` has no route for from the UI in `dir`.
pub(crate) fn serve(router: Router, dir: &Path) -> Router {
    let ui = Ui {
        files: ServeDir::new(dir).append_index_html_on_directories(false),
        document: ServeFile::new(dir.join("index.html")),
    };
    router.fallback(move |request: Request| respond(ui.clone(), request))
}

async fn respond(ui: Ui, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    if is_api(&path) {
        return erpcraft_api::not_found().await;
    }
    if is_hidden(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let method = request.method().clone();
    let headers = request.headers().clone();
    let relative = path.trim_start_matches('/');
    if relative.is_empty() || relative == "index.html" {
        return document(ui, method, headers).await;
    }

    let is_asset = relative.starts_with("assets/");
    let mut response = into_response(ui.files.clone().oneshot(request).await);
    if response.status() == StatusCode::NOT_FOUND {
        // A missing asset stays missing: a page in place of a script would
        // only fail later and less clearly.
        if is_asset || !(method == Method::GET || method == Method::HEAD) {
            return response;
        }
        return document(ui, method, headers).await;
    }
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if is_asset { IMMUTABLE } else { REVALIDATE }),
    );
    response
}

/// `index.html`, with the headers of an HTML document.
async fn document(ui: Ui, method: Method, headers: HeaderMap) -> Response {
    let mut request = Request::new(Body::empty());
    *request.method_mut() = method;
    *request.headers_mut() = headers;
    let mut response = into_response(ui.document.oneshot(request).await);
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        let headers = response.headers_mut();
        for (name, value) in [
            (CACHE_CONTROL, REVALIDATE),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (REFERRER_POLICY, "same-origin"),
            (X_FRAME_OPTIONS, "DENY"),
            (CONTENT_SECURITY_POLICY, CSP),
        ] {
            headers.insert(name, HeaderValue::from_static(value));
        }
    }
    response
}

fn into_response<B>(result: Result<Response<B>, Infallible>) -> Response
where
    B: axum::body::HttpBody<Data = axum::body::Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    match result {
        Ok(response) => response.map(Body::new),
        Err(never) => match never {},
    }
}

/// Paths that belong to the API or to MCP. They are never answered with a
/// page, so a client that mistypes a path gets a JSON error.
fn is_api(path: &str) -> bool {
    ["/api", "/mcp"].iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Whether a request path names a dotfile or tries to leave the directory.
///
/// The file service refuses `..` by itself. This check is on the path as
/// sent, so it also refuses what the service would decode: a segment that
/// begins with a dot, plain or encoded, and an encoded separator, which
/// could hide such a segment.
fn is_hidden(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains('\\')
        || path.contains("%2f")
        || path.contains("%5c")
        || path
            .split('/')
            .any(|segment| segment.starts_with('.') || segment.starts_with("%2e"))
}

#[cfg(test)]
mod tests {
    use super::{is_api, is_hidden};

    #[test]
    fn api_paths_are_recognized_by_whole_segments() {
        for path in ["/api", "/api/", "/api/nope", "/mcp", "/mcp/x"] {
            assert!(is_api(path), "{path}");
        }
        for path in ["/", "/apis", "/api-keys", "/mcpx", "/accounts/api"] {
            assert!(!is_api(path), "{path}");
        }
    }

    #[test]
    fn dotfiles_and_traversal_are_hidden() {
        for path in [
            "/.env",
            "/../Cargo.toml",
            "/assets/../../secret",
            "/assets/.hidden",
            "/%2e%2e/secret",
            "/%2E%2e/secret",
            "/assets/%2eenv",
            "/assets%2f.env",
            "/assets%2F..%2Fsecret",
            "/assets%5c..%5csecret",
            "/assets\\..\\secret",
        ] {
            assert!(is_hidden(path), "{path}");
        }
        for path in ["/", "/accounts/123", "/assets/app-abc123.js", "/a.b/c.d"] {
            assert!(!is_hidden(path), "{path}");
        }
    }
}
