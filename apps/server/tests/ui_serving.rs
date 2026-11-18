//! The admin UI, served by the same router as the API.

mod common;

use std::fs;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use common::TestApp;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const INDEX: &str = "<!doctype html><title>erpcraft</title><div id=\"root\"></div>";
const SCRIPT: &str = "console.log('app');";
const SECRET: &str = "TOP-SECRET";

/// A directory with a built UI in `ui/` and, next to it, a file that must
/// never be served.
struct Site {
    root: PathBuf,
    ui: PathBuf,
}

impl Site {
    fn new() -> Site {
        let root = std::env::temp_dir().join(format!("erpcraft-ui-{}", Uuid::new_v4().simple()));
        let ui = root.join("ui");
        fs::create_dir_all(ui.join("assets")).unwrap();
        fs::write(ui.join("index.html"), INDEX).unwrap();
        fs::write(ui.join("assets/app-abc123.js"), SCRIPT).unwrap();
        fs::write(
            ui.join("favicon.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        )
        .unwrap();
        fs::write(ui.join(".env"), SECRET).unwrap();
        fs::write(ui.join("assets/.hidden"), SECRET).unwrap();
        fs::write(root.join("secret.txt"), SECRET).unwrap();
        Site { root, ui }
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn send(app: &Router, method: &str, path: &str) -> (StatusCode, HeaderMap, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

async fn get(app: &Router, path: &str) -> (StatusCode, HeaderMap, String) {
    send(app, "GET", path).await
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .unwrap_or_else(|| panic!("no {name} header"))
        .to_str()
        .unwrap()
}

#[tokio::test]
async fn the_ui_is_served_next_to_the_api() {
    let t = TestApp::start().await;
    let site = Site::new();
    let app = erpcraft_server::app_with_ui(t.kernel.clone(), Some(&site.ui));

    // The document, at the root and at any path the UI routes itself.
    for path in [
        "/",
        "/accounts/123",
        "/index.html",
        "/apis",
        "/journal_entry/new?x=1",
    ] {
        let (status, headers, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body, INDEX, "{path}");
        assert!(
            header(&headers, "content-type").starts_with("text/html"),
            "{path}"
        );
        assert_eq!(header(&headers, "cache-control"), "no-cache", "{path}");
        assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(header(&headers, "referrer-policy"), "same-origin");
        assert_eq!(header(&headers, "x-frame-options"), "DENY");
        let csp = header(&headers, "content-security-policy");
        assert!(csp.contains("default-src 'self'"), "{csp}");
        assert!(csp.contains("script-src 'self';"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        // Scripts never come from anywhere but this server.
        let scripts = csp
            .split(';')
            .find(|d| d.trim().starts_with("script-src"))
            .unwrap();
        assert!(!scripts.contains("unsafe"), "{csp}");
    }
    let (status, headers, body) = send(&app, "HEAD", "/accounts/123").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "");
    assert_eq!(header(&headers, "cache-control"), "no-cache");

    // A hashed asset may be kept forever.
    let (status, headers, body) = get(&app, "/assets/app-abc123.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, SCRIPT);
    let content_type = header(&headers, "content-type");
    assert!(content_type.contains("javascript"), "{content_type}");
    let cache = header(&headers, "cache-control");
    assert!(
        cache.contains("immutable") && cache.contains("max-age=31536000"),
        "{cache}"
    );
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    // A missing asset is missing, not a page.
    let (status, _, body) = get(&app, "/assets/app-old999.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(body, INDEX);
    // Other files are served, but not kept without asking.
    let (status, headers, _) = get(&app, "/favicon.svg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), "image/svg+xml");
    assert_eq!(header(&headers, "cache-control"), "no-cache");

    // The API still answers in JSON, also where it has no route.
    for path in [
        "/api/nope",
        "/api",
        "/api/",
        "/mcp/nope",
        "/api/assets/x.js",
    ] {
        let (status, headers, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(header(&headers, "content-type"), "application/json");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert!(body["error"].is_object(), "{path}: {body}");
    }
    let (status, _, body) = get(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({ "status": "ok" })
    );
    let (status, _, _) = get(&app, "/api/meta").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, meta) = {
        let request = Request::builder()
            .uri("/api/meta?module=gl")
            .header("authorization", format!("Bearer {}", t.admin))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice::<Value>(&bytes).unwrap())
    };
    assert_eq!(status, StatusCode::OK, "{meta}");
    assert_eq!(meta["modules"][0]["name"], "gl");
    // A route keeps its own methods; only unknown paths reach the UI.
    let (status, _, _) = get(&app, "/mcp").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _, body) = send(&app, "POST", "/accounts/123").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_ne!(body, INDEX);

    // Nothing outside the directory and no dotfile is ever served.
    for path in [
        "/../secret.txt",
        "/assets/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2E%2E%2Fsecret.txt",
        "/..%2fsecret.txt",
        "/assets/..%5c..%5csecret.txt",
        "/../Cargo.toml",
        "/.env",
        "/%2eenv",
        "/assets/.hidden",
        "/assets%2f.hidden",
        "//.env",
    ] {
        let (status, _, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(!body.contains(SECRET), "{path} leaked a file");
        assert_ne!(body, INDEX, "{path}");
    }

    t.stop().await;
}

#[tokio::test]
async fn without_a_ui_the_root_is_the_json_index() {
    let t = TestApp::start().await;
    for app in [
        erpcraft_server::app(t.kernel.clone()),
        erpcraft_server::app_with_ui(t.kernel.clone(), None),
    ] {
        let (status, headers, body) = get(&app, "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header(&headers, "content-type"), "application/json");
        let index: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(index["name"], "erpcraft");
        assert_eq!(index["endpoints"]["health"], "GET /healthz");

        for path in ["/accounts/123", "/api/nope", "/assets/app-abc123.js"] {
            let (status, headers, body) = get(&app, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(header(&headers, "content-type"), "application/json");
            let body: Value = serde_json::from_str(&body).unwrap();
            assert!(body["error"].is_object(), "{path}: {body}");
        }
        let (status, _, _) = get(&app, "/healthz").await;
        assert_eq!(status, StatusCode::OK);
    }
    t.stop().await;
}
