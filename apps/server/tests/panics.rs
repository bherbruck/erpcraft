//! A handler that panics is answered as any internal error is: a JSON 500,
//! not a dropped connection.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn boom() -> &'static str {
    panic!("a handler failed in a way nobody planned for")
}

#[tokio::test]
async fn a_panicking_handler_answers_with_the_json_error() {
    let app = erpcraft_server::catch_panics(Router::new().route("/boom", get(boom)));
    let response = app
        .oneshot(Request::get("/boom").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "error": { "code": "internal", "message": "internal error" } })
    );
}
