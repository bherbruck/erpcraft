//! Single sign-on with OpenID Connect, against an identity provider that
//! runs inside the test.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{PASSWORD, TestApp};
use http_body_util::BodyExt;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const PUBLIC_URL: &str = "http://127.0.0.1:8600";
const CLIENT_ID: &str = "erpcraft-client";
const CLIENT_SECRET: &str = "s3cr3t/known+only:to=the&provider";

/// The settings are read from the environment when a server starts, and
/// the environment belongs to the whole test process: one start at a time.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A server that users reach at [`PUBLIC_URL`], and that accepts a
/// provider on this machine over plain http.
async fn start() -> TestApp {
    start_at(PUBLIC_URL).await
}

/// A server that users reach at `public_url`.
async fn start_at(public_url: &str) -> TestApp {
    let _one_at_a_time = STARTING.lock().await;
    // SAFETY: every test of this file starts its server here, under the
    // lock, and nothing else in the process reads these variables.
    unsafe {
        std::env::set_var(erpcraft_auth::PUBLIC_URL, public_url);
        std::env::set_var(erpcraft_auth::ALLOW_INSECURE, "1");
    }
    TestApp::start().await
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ------------------------------------------------------ the identity provider

#[derive(Clone)]
struct SigningKey {
    kid: String,
    alg: Algorithm,
    key: EncodingKey,
}

impl SigningKey {
    fn ec(kid: &str) -> SigningKey {
        let pair = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
        SigningKey {
            kid: kid.to_owned(),
            alg: Algorithm::ES256,
            key: EncodingKey::from_ec_der(pair.to_pkcs8v1().unwrap().as_ref()),
        }
    }

    fn rsa(kid: &str) -> SigningKey {
        let pair = aws_lc_rs::rsa::KeyPair::generate(KeySize::Rsa2048).unwrap();
        let pkcs8: aws_lc_rs::encoding::Pkcs8V1Der = pair.as_der().unwrap();
        // The key generator writes PKCS#8 and the JWT library reads the
        // bare RSAPrivateKey inside it. For a 2048-bit key that is what
        // follows a header of 26 bytes, the last four of which open the
        // octet string that holds it.
        let pkcs8 = pkcs8.as_ref();
        assert_eq!(pkcs8[22..24], [0x04, 0x82]);
        SigningKey {
            kid: kid.to_owned(),
            alg: Algorithm::RS256,
            key: EncodingKey::from_rsa_der(&pkcs8[26..]),
        }
    }

    fn jwk(&self) -> Jwk {
        let mut jwk = Jwk::from_encoding_key(&self.key, self.alg).unwrap();
        jwk.common.key_id = Some(self.kid.clone());
        jwk
    }

    /// An ID token with these claims, saying it was signed by `kid`.
    fn sign_as(&self, kid: &str, claims: &Value) -> String {
        let mut header = Header::new(self.alg);
        header.kid = Some(kid.to_owned());
        jsonwebtoken::encode(&header, claims, &self.key).unwrap()
    }

    fn sign(&self, claims: &Value) -> String {
        self.sign_as(&self.kid, claims)
    }
}

/// What the provider hands out for one authorization code.
struct Grant {
    challenge: String,
    redirect_uri: String,
    id_token: String,
}

#[derive(Default)]
struct IdpState {
    /// The keys it publishes.
    keys: Vec<SigningKey>,
    grants: HashMap<String, Grant>,
    key_set_reads: usize,
    token_requests: usize,
}

/// An OpenID provider in this process: discovery document, token endpoint
/// and key set. Its authorization endpoint is the test itself, which
/// decides what the provider says about who signed in.
#[derive(Clone)]
struct Idp {
    issuer: String,
    state: Arc<Mutex<IdpState>>,
}

async fn discovery(State(idp): State<Idp>) -> Json<Value> {
    Json(json!({
        "issuer": idp.issuer,
        "authorization_endpoint": format!("{}/authorize", idp.issuer),
        "token_endpoint": format!("{}/token", idp.issuer),
        "jwks_uri": format!("{}/keys", idp.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256", "RS256"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn keys(State(idp): State<Idp>) -> Json<Value> {
    let mut state = idp.state.lock().unwrap();
    state.key_set_reads += 1;
    let keys: Vec<Jwk> = state.keys.iter().map(SigningKey::jwk).collect();
    Json(json!({ "keys": keys }))
}

fn oauth_error(status: StatusCode, error: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": error })))
}

/// The token endpoint, as strict as a real one: the client must prove
/// itself, the code is good once, and the PKCE verifier must fit the
/// challenge the authorization request carried.
async fn token(
    State(idp): State<Idp>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let mut state = idp.state.lock().unwrap();
    state.token_requests += 1;

    let encode =
        |part: &str| url::form_urlencoded::byte_serialize(part.as_bytes()).collect::<String>();
    let expected = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", encode(CLIENT_ID), encode(CLIENT_SECRET)))
    );
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    if authorization != Some(expected.as_str()) {
        return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    if form.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    let Some(grant) = form.get("code").and_then(|code| state.grants.remove(code)) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if challenge != grant.challenge || form.get("redirect_uri") != Some(&grant.redirect_uri) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    (
        StatusCode::OK,
        Json(
            json!({ "access_token": "unused", "token_type": "Bearer", "id_token": grant.id_token }),
        ),
    )
}

impl Idp {
    async fn start() -> Idp {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp = Idp {
            issuer: format!("http://{}", listener.local_addr().unwrap()),
            state: Arc::default(),
        };
        idp.state.lock().unwrap().keys.push(SigningKey::ec("key-1"));
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/keys", get(keys))
            .route("/token", post(token))
            .with_state(idp.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        idp
    }

    fn key(&self) -> SigningKey {
        self.state.lock().unwrap().keys[0].clone()
    }

    /// Replace the published keys, as a provider does from time to time.
    fn rotate(&self, key: SigningKey) {
        self.state.lock().unwrap().keys = vec![key];
    }

    fn key_set_reads(&self) -> usize {
        self.state.lock().unwrap().key_set_reads
    }

    fn token_requests(&self) -> usize {
        self.state.lock().unwrap().token_requests
    }

    /// What this provider would say about `email` for the sign-in `started`.
    fn claims(&self, started: &Started, email: &str) -> Value {
        json!({
            "iss": self.issuer,
            // The same person whatever the case of the address.
            "sub": format!("subject-of-{}", email.to_lowercase()),
            "aud": CLIENT_ID,
            "exp": now() + 300,
            "iat": now(),
            "nonce": started.nonce,
            "email": email,
            "email_verified": true,
            "name": "Sam Sample",
        })
    }

    /// The user has signed in at the provider: issue an authorization code
    /// that the token endpoint will exchange for `id_token`.
    fn authorize(&self, started: &Started, id_token: String) -> String {
        let code = Uuid::new_v4().simple().to_string();
        self.state.lock().unwrap().grants.insert(
            code.clone(),
            Grant {
                challenge: started.challenge.clone(),
                redirect_uri: started.redirect_uri.clone(),
                id_token,
            },
        );
        code
    }
}

// ------------------------------------------------------------- the browser

async fn call(
    t: &TestApp,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let body = match body {
        Some(value) => Body::from(value.to_string()),
        None => Body::empty(),
    };
    let response = t
        .app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

fn location(headers: &HeaderMap) -> Url {
    Url::parse(headers["location"].to_str().unwrap()).unwrap()
}

fn param(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// A sign-in that has been sent to the provider.
struct Started {
    slug: String,
    state: String,
    nonce: String,
    challenge: String,
    redirect_uri: String,
    /// The browser's cookie, as `name=value`.
    cookie: String,
    /// Where the browser was sent.
    authorize: Url,
}

async fn begin_at(t: &TestApp, slug: &str, return_to: Option<&str>) -> Started {
    let path = match return_to {
        Some(return_to) => {
            let encoded: String =
                url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
            format!("/api/auth/sso/{slug}/start?return_to={encoded}")
        }
        None => format!("/api/auth/sso/{slug}/start"),
    };
    let (status, headers, body) = call(t, "GET", &path, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    let authorize = location(&headers);
    let cookie = headers["set-cookie"].to_str().unwrap();
    Started {
        slug: slug.to_owned(),
        state: param(&authorize, "state").unwrap(),
        nonce: param(&authorize, "nonce").unwrap(),
        challenge: param(&authorize, "code_challenge").unwrap(),
        redirect_uri: param(&authorize, "redirect_uri").unwrap(),
        cookie: cookie.split(';').next().unwrap().to_owned(),
        authorize,
    }
}

async fn begin(t: &TestApp, slug: &str) -> Started {
    begin_at(t, slug, None).await
}

/// Where the browser lands after the provider sent it back.
struct Landing {
    url: Url,
    cookie: String,
}

impl Landing {
    /// The word the sign-in page is given for a refused sign-in.
    fn error(&self) -> Option<String> {
        assert!(param(&self.url, "sso_code").is_none() || param(&self.url, "sso_error").is_none());
        param(&self.url, "sso_error")
    }

    fn code(&self) -> String {
        param(&self.url, "sso_code").unwrap_or_else(|| panic!("no login code in {}", self.url))
    }
}

/// The provider sends the browser back with `code`.
async fn come_back(t: &TestApp, started: &Started, code: &str, cookie: Option<&str>) -> Landing {
    let path = format!(
        "/api/auth/sso/{}/callback?code={code}&state={}",
        started.slug, started.state
    );
    let (status, headers, body) = call(t, "GET", &path, cookie, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert_eq!(headers["cache-control"], "no-store");
    Landing {
        url: location(&headers),
        cookie: started.cookie.clone(),
    }
}

/// A whole sign-in of `email` through `slug`, with the claims changed by
/// `tweak` and the ID token signed by `sign`.
async fn sign_in_with(
    t: &TestApp,
    idp: &Idp,
    slug: &str,
    email: &str,
    tweak: impl FnOnce(&mut Value),
    sign: impl FnOnce(&Value) -> String,
) -> Landing {
    let started = begin(t, slug).await;
    let mut claims = idp.claims(&started, email);
    tweak(&mut claims);
    let code = idp.authorize(&started, sign(&claims));
    come_back(t, &started, &code, Some(&started.cookie)).await
}

async fn sign_in(t: &TestApp, idp: &Idp, slug: &str, email: &str) -> Landing {
    let key = idp.key();
    sign_in_with(t, idp, slug, email, |_| {}, |claims| key.sign(claims)).await
}

/// Collect the token for a login code.
async fn exchange(t: &TestApp, code: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let (status, _, body) = call(
        t,
        "POST",
        "/api/auth/sso/exchange",
        cookie,
        Some(json!({ "code": code })),
    )
    .await;
    (status, body)
}

/// A provider record pointing at `idp`, for the domain example.com unless
/// `more` says otherwise.
async fn provider(t: &TestApp, idp: &Idp, slug: &str, more: Value) -> Value {
    let mut input = json!({
        "name": format!("Provider {slug}"),
        "slug": slug,
        "issuer": idp.issuer,
        "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET,
        "allowed_domains": ["example.com"],
    });
    input
        .as_object_mut()
        .unwrap()
        .extend(more.as_object().unwrap().clone());
    let (status, body) = t.command(&t.admin, "sso_provider.create", input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn user(t: &TestApp, email: &str) -> String {
    let (status, body) = t
        .command(
            &t.admin,
            "user.create",
            json!({ "email": email, "name": "Existing User", "password": PASSWORD }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

async fn events(t: &TestApp, input: Value) -> Vec<Value> {
    let (status, body) = t.command(&t.admin, "auth.events", input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["items"].as_array().unwrap().clone()
}

/// The reason of the latest refused sign-in.
async fn last_refusal(t: &TestApp) -> String {
    let refused = events(t, json!({ "outcome": "refused", "limit": 1 })).await;
    refused[0]["reason"].as_str().unwrap().to_owned()
}

// -------------------------------------------------------------------- tests

#[tokio::test]
async fn an_existing_user_signs_in_through_a_provider() {
    let t = start().await;
    let idp = Idp::start().await;
    let created = provider(&t, &idp, "idp", json!({})).await;
    assert_eq!(
        created["redirect_uri"],
        format!("{PUBLIC_URL}/api/auth/sso/idp/callback")
    );
    assert_eq!(created["scopes"], "openid email profile");
    assert_eq!(created["auto_create_users"], false);
    assert_eq!(created["require_verified_email"], true);
    assert!(created.get("client_secret").is_none());
    let alice = user(&t, "alice@example.com").await;

    // The sign-in page learns which providers to offer, without a token.
    let (status, _, body) = call(&t, "GET", "/api/auth/sso", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "providers": [{ "slug": "idp", "name": "Provider idp" }] })
    );

    // Start: the browser is sent to the provider with everything the
    // authorization code flow with PKCE needs, and is given a cookie.
    let (_, headers, _) = call(
        &t,
        "GET",
        "/api/auth/sso/idp/start?return_to=%2Fc%2Facme%3Ftab%3D1",
        None,
        None,
    )
    .await;
    let set_cookie = headers["set-cookie"].to_str().unwrap().to_owned();
    // Plain http on this machine: no `__Host-` prefix, which needs Secure.
    assert!(set_cookie.starts_with("erpcraft_sso_"), "{set_cookie}");
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/;", "Max-Age=600"] {
        assert!(set_cookie.contains(attribute), "{set_cookie}");
    }
    assert!(!set_cookie.contains("Domain"), "{set_cookie}");
    assert_eq!(headers["cache-control"], "no-store");

    let started = begin_at(&t, "idp", Some("/c/acme?tab=1")).await;
    let authorize = &started.authorize;
    assert!(
        authorize
            .as_str()
            .starts_with(&format!("{}/authorize?", idp.issuer))
    );
    assert_eq!(param(authorize, "response_type").unwrap(), "code");
    assert_eq!(param(authorize, "client_id").unwrap(), CLIENT_ID);
    assert_eq!(param(authorize, "scope").unwrap(), "openid email profile");
    assert_eq!(param(authorize, "code_challenge_method").unwrap(), "S256");
    // Built from the configured public URL, not from the request.
    assert_eq!(
        started.redirect_uri,
        format!("{PUBLIC_URL}/api/auth/sso/idp/callback")
    );
    assert!(started.state.len() >= 43 && started.nonce.len() >= 43);
    assert_ne!(started.state, started.nonce);
    // Neither the state nor the browser's cookie is stored as it is.
    let cookie_value = started.cookie.split_once('=').unwrap().1;
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM sso_login_attempt a
             WHERE to_jsonb(a)::text LIKE '%{}%' OR to_jsonb(a)::text LIKE '%{cookie_value}%'",
            started.state
        ))
        .await,
        0
    );

    // The provider sends the browser back; it lands where it wanted to go
    // with a login code, and no token in the address.
    let code = idp.authorize(
        &started,
        idp.key().sign(&idp.claims(&started, "Alice@Example.com")),
    );
    let landing = come_back(&t, &started, &code, Some(&started.cookie)).await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    assert!(
        landing
            .url
            .as_str()
            .starts_with(&format!("{PUBLIC_URL}/c/acme?tab=1&sso_code=")),
        "{}",
        landing.url
    );
    assert!(!landing.url.as_str().contains("erp_"));
    assert_eq!(idp.token_requests(), 1);
    let login_code = landing.code();
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM sso_login_code c WHERE to_jsonb(c)::text LIKE '%{login_code}%'"
        ))
        .await,
        0,
        "the login code is stored hashed"
    );

    // The page exchanges the code for the same thing a password gives.
    let (status, session) = exchange(&t, &login_code, Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let token = session["token"].as_str().unwrap();
    assert!(token.starts_with("erp_"));
    assert!(session["expires_at"].is_string());
    let (status, me) = t.get(token, "/api/auth/me").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["email"], "alice@example.com");
    assert_eq!(me["user_id"], alice.as_str());
    assert_eq!(
        t.count("SELECT count(*) FROM api_token WHERE signed_in_with = 'sso:idp'")
            .await,
        1
    );
    assert_eq!(t.count("SELECT count(*) FROM sso_login_attempt").await, 1);
    assert_eq!(t.count("SELECT count(*) FROM sso_login_code").await, 0);

    // A login code is good once.
    let (status, _) = exchange(&t, &login_code, Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // And so is a state: the same answer from the provider a second time.
    let again = come_back(&t, &started, &code, Some(&started.cookie)).await;
    assert_eq!(again.error().as_deref(), Some("invalid_state"));
    assert_eq!(again.url.path(), "/sign-in");
    assert_eq!(idp.token_requests(), 1, "nothing was asked of the provider");

    // Password sign-in works alongside, for the same user.
    let (status, _, session) = call(
        &t,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "email": "alice@example.com", "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    assert_eq!(
        t.count("SELECT count(*) FROM api_token WHERE signed_in_with = 'password'")
            .await,
        // The administrator of the harness signed in with a password too.
        2
    );
    let (status, _, _) = call(
        &t,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "email": "alice@example.com", "password": "not-the-password" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // An administrator sees who signed in, how, and what was refused.
    let all = events(&t, json!({})).await;
    let summary: Vec<(&str, &str, &str)> = all
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap(),
                e["outcome"].as_str().unwrap(),
                e["email"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("password", "refused", "alice@example.com"),
            ("password", "success", "alice@example.com"),
            ("sso", "refused", ""),
            ("sso", "success", "alice@example.com"),
            // The first sign-in bound Alice to the identity the provider
            // stated.
            ("sso_identity", "success", "alice@example.com"),
            ("sso_provider", "success", common::ADMIN_EMAIL),
            ("password", "success", common::ADMIN_EMAIL),
        ]
    );
    assert_eq!(all[3]["subject"], "subject-of-alice@example.com");
    assert_eq!(
        all[4]["reason"],
        "bound at first sign-in to subject subject-of-alice@example.com"
    );
    assert_eq!(all[3]["provider"], "idp");
    assert_eq!(all[3]["user_id"], alice.as_str());
    assert_eq!(all[3]["reason"], Value::Null);
    assert_eq!(all[2]["reason"], "the state is unknown or was used before");
    assert_eq!(all[0]["reason"], "wrong email address or password");
    assert_eq!(all[0]["user_id"], alice.as_str());
    let by_provider = events(
        &t,
        json!({ "provider": "idp", "outcome": "success", "kind": "sso" }),
    )
    .await;
    assert_eq!(by_provider.len(), 1);
    let by_email = events(&t, json!({ "email": "Alice@example.com" })).await;
    assert_eq!(by_email.len(), 4);
    // Nothing in the record could be used to sign in.
    let record = Value::from(all).to_string();
    for secret in [
        started.state.as_str(),
        login_code.as_str(),
        token,
        code.as_str(),
        PASSWORD,
    ] {
        assert!(!record.contains(secret));
    }
    // The record is for those who may read it.
    let clerk = t
        .user_with_role(
            "clerk@example.com",
            &["account:read"],
            &t.company("ACME", "USD").await.id,
        )
        .await;
    let (status, _) = t.command(&clerk, "auth.events", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    t.stop().await;
}

#[tokio::test]
async fn states_and_login_codes_expire_and_belong_to_one_browser() {
    let t = start().await;
    let idp = Idp::start().await;
    provider(&t, &idp, "idp", json!({})).await;
    provider(&t, &idp, "other", json!({})).await;
    user(&t, "alice@example.com").await;
    let key = idp.key();
    let token_for = |started: &Started| key.sign(&idp.claims(started, "alice@example.com"));

    // A sign-in that took too long.
    let started = begin(&t, "idp").await;
    sqlx::query("UPDATE sso_login_attempt SET expires_at = now() - interval '1 second'")
        .execute(&t.pool)
        .await
        .unwrap();
    let code = idp.authorize(&started, token_for(&started));
    let landing = come_back(&t, &started, &code, Some(&started.cookie)).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));
    assert!(last_refusal(&t).await.contains("expired"));
    assert_eq!(t.count("SELECT count(*) FROM sso_login_attempt").await, 0);

    // Somebody else's finished sign-in, opened in another browser: a link
    // like this is how one person would be signed in as another.
    let started = begin(&t, "idp").await;
    let code = idp.authorize(&started, token_for(&started));
    let landing = come_back(&t, &started, &code, None).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));
    assert!(
        last_refusal(&t)
            .await
            .contains("not started in this browser")
    );
    let started = begin(&t, "idp").await;
    let code = idp.authorize(&started, token_for(&started));
    let landing = come_back(&t, &started, &code, Some("erpcraft_sso_00000000=guessed")).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));

    // A state made up, a state missing, and a state of another provider.
    let (status, headers, _) = call(
        &t,
        "GET",
        "/api/auth/sso/idp/callback?code=x&state=made-up",
        Some(&started.cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        param(&location(&headers), "sso_error").as_deref(),
        Some("invalid_state")
    );
    let (_, headers, _) = call(&t, "GET", "/api/auth/sso/idp/callback?code=x", None, None).await;
    assert_eq!(
        param(&location(&headers), "sso_error").as_deref(),
        Some("invalid_state")
    );
    let mut started = begin(&t, "idp").await;
    let code = idp.authorize(&started, token_for(&started));
    started.slug = "other".into();
    let landing = come_back(&t, &started, &code, Some(&started.cookie)).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));
    assert_eq!(idp.token_requests(), 0, "no code was ever exchanged");

    // The person said no at the provider. The refusal is recorded once
    // the state shows that the answer belongs to a sign-in started here.
    let started = begin_at(&t, "idp", Some("/c/acme")).await;
    let (_, headers, _) = call(
        &t,
        "GET",
        &format!(
            "/api/auth/sso/idp/callback?error=access_denied&error_description=no+thanks&state={}",
            started.state
        ),
        Some(&started.cookie),
        None,
    )
    .await;
    let landed = location(&headers);
    assert_eq!(
        param(&landed, "sso_error").as_deref(),
        Some("provider_refused")
    );
    assert_eq!(param(&landed, "return_to").as_deref(), Some("/c/acme"));
    assert_eq!(
        last_refusal(&t).await,
        "the provider answered access_denied: no thanks"
    );

    // A login code that waited too long, and one in another browser.
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    sqlx::query("UPDATE sso_login_code SET expires_at = now() - interval '1 second'")
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, _) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(last_refusal(&t).await, "the login code has expired");
    assert_eq!(t.count("SELECT count(*) FROM sso_login_code").await, 0);

    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    let (status, _) = exchange(&t, &landing.code(), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The attempt used the code up: the right browser is too late.
    let (status, _) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = exchange(&t, "made-up", Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = call(&t, "POST", "/api/auth/sso/exchange", None, Some(json!({}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        t.count("SELECT count(*) FROM api_token WHERE signed_in_with LIKE 'sso:%'")
            .await,
        0
    );

    t.stop().await;
}

#[tokio::test]
async fn return_to_is_a_local_path_and_the_provider_must_be_active() {
    let t = start().await;
    let idp = Idp::start().await;
    let created = provider(&t, &idp, "idp", json!({})).await;

    for return_to in [
        "https://evil.example/",
        "http://evil.example/",
        "//evil.example/path",
        "/\\evil.example",
        "evil.example",
        "javascript:alert(1)",
    ] {
        let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
        let (status, headers, body) = call(
            &t,
            "GET",
            &format!("/api/auth/sso/idp/start?return_to={encoded}"),
            None,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{return_to}: {body}"
        );
        assert_eq!(body["error"]["field"], "return_to", "{body}");
        assert!(headers.get("location").is_none());
    }
    assert_eq!(t.count("SELECT count(*) FROM sso_login_attempt").await, 0);

    let (status, _, _) = call(&t, "GET", "/api/auth/sso/nope/start", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Nobody signs in through an inactive provider, and it is not offered.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": created["id"], "active": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active"], false);
    let (status, _, _) = call(&t, "GET", "/api/auth/sso/idp/start", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, body) = call(&t, "GET", "/api/auth/sso", None, None).await;
    assert_eq!(body, json!({ "providers": [] }));

    // A provider that cannot be reached: the browser goes back to the
    // sign-in page, and the administrator can see why.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone = Idp {
        issuer: format!("http://{}", dead.local_addr().unwrap()),
        state: Arc::default(),
    };
    drop(dead);
    provider(&t, &gone, "gone", json!({})).await;
    let (status, headers, _) = call(
        &t,
        "GET",
        "/api/auth/sso/gone/start?return_to=%2Fc%2Facme",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let landed = location(&headers);
    assert_eq!(landed.path(), "/sign-in");
    assert_eq!(
        param(&landed, "sso_error").as_deref(),
        Some("provider_unavailable")
    );
    assert_eq!(param(&landed, "return_to").as_deref(), Some("/c/acme"));
    assert!(last_refusal(&t).await.contains("discovery document"));

    // A provider that states another issuer than the one configured.
    let liar = Idp {
        issuer: format!("{}/", idp.issuer),
        state: Arc::default(),
    };
    provider(&t, &liar, "liar", json!({})).await;
    let (_, headers, _) = call(&t, "GET", "/api/auth/sso/liar/start", None, None).await;
    assert_eq!(
        param(&location(&headers), "sso_error").as_deref(),
        Some("provider_unavailable")
    );
    assert!(last_refusal(&t).await.contains("states its issuer as"));

    // Only https issuers beyond this machine; and what a provider must have.
    for (change, field) in [
        (json!({ "issuer": "http://login.example.com" }), "issuer"),
        (
            json!({ "issuer": "https://user:pw@login.example.com" }),
            "issuer",
        ),
        (json!({ "issuer": "login.example.com" }), "issuer"),
        (json!({ "scopes": "email profile" }), "scopes"),
        (json!({ "scopes": "openid profile" }), "scopes"),
        (json!({ "slug": "Not A Slug" }), "slug"),
        (json!({ "slug": "exchange" }), "slug"),
        (json!({ "client_secret": "" }), "client_secret"),
        (
            json!({ "allowed_domains": ["not a domain"] }),
            "allowed_domains",
        ),
        // Creating users for any address a public provider vouches for
        // would make everybody a user.
        (
            json!({ "auto_create_users": true, "allowed_domains": ["*"] }),
            "allowed_domains",
        ),
        // Any domain is said, never meant by leaving the domains out.
        (json!({ "allowed_domains": [] }), "allowed_domains"),
        (
            json!({ "allowed_domains": ["*", "example.com"] }),
            "allowed_domains",
        ),
        (
            json!({ "default_role_id": Uuid::new_v4() }),
            "default_role_id",
        ),
        (
            json!({ "default_company_id": Uuid::new_v4() }),
            "default_company_id",
        ),
    ] {
        let mut input = json!({
            "name": "X", "slug": "x", "issuer": "https://login.example.com",
            "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
            "allowed_domains": ["example.com"],
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        let (status, body) = t.command(&t.admin, "sso_provider.create", input).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{change}: {body}");
        assert_eq!(body["error"]["field"], field, "{change}: {body}");
    }
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.create",
            json!({ "name": "Again", "slug": "idp", "issuer": idp.issuer,
                    "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
                    "allowed_domains": ["example.com"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // Leaving the domains out is not "any domain": it is refused.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.create",
            json!({ "name": "Y", "slug": "y", "issuer": idp.issuer,
                    "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "allowed_domains", "{body}");

    t.stop().await;
}

#[tokio::test]
async fn a_first_sign_in_creates_a_user_only_where_the_provider_allows() {
    let t = start().await;
    let idp = Idp::start().await;
    let c = t.company("ACME", "USD").await;
    let role = t
        .create(
            "role",
            json!({ "name": "Newcomer", "permissions": ["account:read"] }),
        )
        .await;
    provider(&t, &idp, "closed", json!({})).await;
    provider(
        &t,
        &idp,
        "open",
        json!({
            "auto_create_users": true,
            "allowed_domains": ["Example.com"],
            "default_role_id": role,
            "default_company_id": c.id,
        }),
    )
    .await;
    let users = "SELECT count(*) FROM app_user";
    let before = t.count(users).await;

    // Not a user, and the provider does not create users.
    let landing = sign_in(&t, &idp, "closed", "newcomer@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("no_account"));
    assert_eq!(t.count(users).await, before);

    // A domain that may not sign in, whatever the provider says.
    let landing = sign_in(&t, &idp, "open", "newcomer@other.org").await;
    assert_eq!(landing.error().as_deref(), Some("domain_not_allowed"));
    assert_eq!(
        last_refusal(&t).await,
        "the domain other.org may not sign in through this provider"
    );
    let landing = sign_in(&t, &idp, "open", "newcomer@example.com.evil.org").await;
    assert_eq!(landing.error().as_deref(), Some("domain_not_allowed"));
    let landing = sign_in(&t, &idp, "open", "example.com@evil.org").await;
    assert_eq!(landing.error().as_deref(), Some("domain_not_allowed"));

    // An address the provider does not vouch for could be anybody's.
    let key = idp.key();
    for unverified in [json!(false), json!("false"), Value::Null] {
        let landing = sign_in_with(
            &t,
            &idp,
            "open",
            "newcomer@example.com",
            |claims| {
                if unverified.is_null() {
                    claims.as_object_mut().unwrap().remove("email_verified");
                } else {
                    claims["email_verified"] = unverified.clone();
                }
            },
            |claims| key.sign(claims),
        )
        .await;
        assert_eq!(
            landing.error().as_deref(),
            Some("email_not_verified"),
            "{unverified}"
        );
    }
    let landing = sign_in_with(
        &t,
        &idp,
        "open",
        "newcomer@example.com",
        |claims| {
            claims.as_object_mut().unwrap().remove("email");
        },
        |claims| key.sign(claims),
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("no_email"));
    assert_eq!(t.count(users).await, before);

    // Allowed: the first sign-in creates the user, with the role.
    let landing = sign_in(&t, &idp, "open", "Newcomer@Example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    assert_eq!(t.count(users).await, before + 1);
    let (status, session) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let token = session["token"].as_str().unwrap().to_owned();
    let (_, me) = t.get(&token, "/api/auth/me").await;
    assert_eq!(me["email"], "newcomer@example.com");
    assert_eq!(me["name"], "Sam Sample");
    assert_eq!(me["is_admin"], false);
    let (status, body) = t.get(&token, "/api/entities/account").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["total"].as_i64().unwrap() > 0);
    let (status, _) = t
        .post(
            &token,
            "/api/entities/account",
            json!({ "company_id": c.id, "code": "9", "name": "N", "account_type": "asset" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM role_assignment ra JOIN app_user u ON u.id = ra.user_id
             WHERE u.email = 'newcomer@example.com' AND ra.role_id = '{role}'
               AND ra.company_id = '{}'",
            c.id
        ))
        .await,
        1
    );

    // The new user has no password, and cannot get in with one.
    assert_eq!(
        t.count(
            "SELECT count(*) FROM user_password p JOIN app_user u ON u.id = p.user_id
             WHERE u.email = 'newcomer@example.com'"
        )
        .await,
        0
    );
    for password in ["", PASSWORD, "null"] {
        let (status, _, _) = call(
            &t,
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "email": "newcomer@example.com", "password": password })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // So is a user an administrator creates without a password, for an
    // installation where everybody signs in through a provider.
    let (status, body) = t
        .command(
            &t.admin,
            "user.create",
            json!({ "email": "carol@example.com", "name": "Carol" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, _) = call(
        &t,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "email": "carol@example.com", "password": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let landing = sign_in(&t, &idp, "closed", "carol@example.com").await;
    assert_eq!(landing.error(), None);

    // The second sign-in finds the user. Another provider does not: the
    // first sign-in bound the newcomer to the provider it went through.
    let landing = sign_in(&t, &idp, "open", "newcomer@example.com").await;
    assert_eq!(landing.error(), None);
    let landing = sign_in(&t, &idp, "closed", "newcomer@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("not_bound"));
    // The newcomer and Carol, each once.
    assert_eq!(t.count(users).await, before + 2);

    // A user who was deactivated does not get in, not even with a login
    // code handed out just before.
    let waiting = sign_in(&t, &idp, "open", "newcomer@example.com").await;
    sqlx::query("UPDATE app_user SET active = false WHERE email = 'newcomer@example.com'")
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, _) = exchange(&t, &waiting.code(), Some(&waiting.cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(last_refusal(&t).await, "the user is inactive");
    let landing = sign_in(&t, &idp, "open", "newcomer@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("inactive"));
    let (status, _) = t.get(&token, "/api/auth/me").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A provider that sends no `email_verified`, such as Microsoft Entra
    // ID: refused, unless it says the domain owner is verified, or the
    // provider is set up as one whose addresses the organization controls.
    let entra_like = |claims: &mut Value| {
        claims.as_object_mut().unwrap().remove("email_verified");
    };
    user(&t, "bob@example.com").await;
    let landing = sign_in_with(&t, &idp, "closed", "bob@example.com", entra_like, |c| {
        key.sign(c)
    })
    .await;
    assert_eq!(landing.error().as_deref(), Some("email_not_verified"));
    let landing = sign_in_with(
        &t,
        &idp,
        "closed",
        "bob@example.com",
        |claims| {
            entra_like(claims);
            claims["xms_edov"] = json!(true);
        },
        |c| key.sign(c),
    )
    .await;
    assert_eq!(landing.error(), None);
    provider(
        &t,
        &idp,
        "trusting",
        json!({ "require_verified_email": false }),
    )
    .await;
    user(&t, "dave@example.com").await;
    let landing = sign_in_with(&t, &idp, "trusting", "dave@example.com", entra_like, |c| {
        key.sign(c)
    })
    .await;
    assert_eq!(landing.error(), None);

    // Naming the role new users get is granting roles: it takes that
    // permission besides.
    let (manager, _) = t
        .user_with_rules(
            "manager@example.com",
            json!(["sso_provider:manage", "sso_provider:read"]),
            None,
        )
        .await;
    let input = json!({
        "name": "Mine", "slug": "mine", "issuer": idp.issuer, "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET, "auto_create_users": true,
        "allowed_domains": ["example.com"],
    });
    let mut with_role = input.clone();
    with_role["default_role_id"] = json!(role);
    let (status, body) = t.command(&manager, "sso_provider.create", with_role).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = t.command(&manager, "sso_provider.create", input).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = t
        .command(
            &manager,
            "sso_provider.update",
            json!({ "id": body["id"], "default_role_id": role }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    t.stop().await;
}

#[tokio::test]
async fn an_id_token_that_is_not_right_is_refused() {
    let t = start().await;
    let idp = Idp::start().await;
    provider(&t, &idp, "idp", json!({})).await;
    user(&t, "alice@example.com").await;
    let key = idp.key();
    let stranger = SigningKey::ec("key-1");
    let issuer = idp.issuer.clone();

    type Tweak = Box<dyn FnOnce(&mut Value)>;
    let cases: Vec<(&str, Tweak, &str)> = vec![
        (
            "wrong nonce",
            Box::new(|c| c["nonce"] = json!("another-sign-in")),
            "nonce",
        ),
        (
            "no nonce",
            Box::new(|c| {
                c.as_object_mut().unwrap().remove("nonce");
            }),
            "nonce",
        ),
        (
            "wrong audience",
            Box::new(|c| c["aud"] = json!("another-client")),
            "another client",
        ),
        (
            "several audiences and no authorized party",
            Box::new(|c| c["aud"] = json!([CLIENT_ID, "another-client"])),
            "another client",
        ),
        (
            "authorized party is another client",
            Box::new(|c| c["azp"] = json!("another-client")),
            "another client",
        ),
        (
            "wrong issuer",
            Box::new(|c| c["iss"] = json!("https://accounts.evil.example")),
            "another issuer",
        ),
        (
            "issuer differs by a slash",
            Box::new(move |c| c["iss"] = json!(format!("{issuer}/"))),
            "another issuer",
        ),
        (
            "expired",
            Box::new(|c| {
                c["iat"] = json!(now() - 400);
                c["exp"] = json!(now() - 100);
            }),
            "expired",
        ),
        (
            "no expiry",
            Box::new(|c| {
                c.as_object_mut().unwrap().remove("exp");
            }),
            "not valid",
        ),
        (
            "issued long ago",
            Box::new(|c| c["iat"] = json!(now() - 3600)),
            "too old",
        ),
        (
            "issued in the future",
            Box::new(|c| c["iat"] = json!(now() + 3600)),
            "future",
        ),
        (
            "not yet valid",
            Box::new(|c| c["nbf"] = json!(now() + 3600)),
            "not valid",
        ),
    ];
    for (name, tweak, reason) in cases {
        let landing =
            sign_in_with(&t, &idp, "idp", "alice@example.com", tweak, |c| key.sign(c)).await;
        assert_eq!(landing.error().as_deref(), Some("invalid_token"), "{name}");
        assert_eq!(landing.url.path(), "/sign-in", "{name}");
        let recorded = last_refusal(&t).await;
        assert!(recorded.contains(reason), "{name}: {recorded}");
    }

    // Signed by somebody who is not the provider, under the provider's
    // key id.
    let landing = sign_in_with(
        &t,
        &idp,
        "idp",
        "alice@example.com",
        |_| {},
        |c| stranger.sign(c),
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("invalid_token"));
    assert_eq!(
        last_refusal(&t).await,
        "the ID token's signature is not valid"
    );

    // Changed after it was signed.
    let landing = sign_in_with(
        &t,
        &idp,
        "idp",
        "alice@example.com",
        |_| {},
        |c| {
            let signed = key.sign(c);
            let mut parts: Vec<&str> = signed.split('.').collect();
            let mut claims = c.clone();
            claims["email"] = json!("admin@example.com");
            let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
            parts[1] = &payload;
            parts.join(".")
        },
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("invalid_token"));
    assert_eq!(
        last_refusal(&t).await,
        "the ID token's signature is not valid"
    );

    // Signed with a shared secret, which anybody could know: the public
    // key itself is the classic choice.
    let landing = sign_in_with(
        &t,
        &idp,
        "idp",
        "alice@example.com",
        |_| {},
        |c| {
            let mut header = Header::new(Algorithm::HS256);
            header.kid = Some("key-1".into());
            let secret = serde_json::to_vec(&key.jwk()).unwrap();
            jsonwebtoken::encode(&header, c, &EncodingKey::from_secret(&secret).unwrap()).unwrap()
        },
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("invalid_token"));
    assert!(last_refusal(&t).await.contains("no public key"));

    // Not signed at all.
    let landing = sign_in_with(
        &t,
        &idp,
        "idp",
        "alice@example.com",
        |_| {},
        |c| {
            let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
            format!("{header}.{}.", URL_SAFE_NO_PAD.encode(c.to_string()))
        },
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("invalid_token"));
    let landing = sign_in_with(
        &t,
        &idp,
        "idp",
        "alice@example.com",
        |_| {},
        |_| "not-a-token".into(),
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("invalid_token"));

    // A code the provider does not know, as from a forged answer.
    let started = begin(&t, "idp").await;
    let landing = come_back(&t, &started, "made-up-code", Some(&started.cookie)).await;
    assert_eq!(landing.error().as_deref(), Some("provider_refused"));
    assert!(last_refusal(&t).await.contains("invalid_grant"));

    // The wrong client secret.
    let (_, list) = t.command(&t.admin, "sso_provider.list", json!({})).await;
    let id = list["items"][0]["id"].clone();
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.set_secret",
            json!({ "id": id, "client_secret": "not-the-secret" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("provider_refused"));
    let recorded = last_refusal(&t).await;
    assert!(recorded.contains("invalid_client"), "{recorded}");
    assert!(!recorded.contains("not-the-secret"));

    // None of it left anything to sign in with.
    assert_eq!(t.count("SELECT count(*) FROM sso_login_code").await, 0);
    assert_eq!(
        t.count("SELECT count(*) FROM api_token WHERE signed_in_with LIKE 'sso:%'")
            .await,
        0
    );
    assert_eq!(
        t.count("SELECT count(*) FROM auth_event WHERE kind = 'sso' AND outcome = 'success'")
            .await,
        0
    );

    // With the secret put right, the same provider signs the user in.
    let (status, _) = t
        .command(
            &t.admin,
            "sso_provider.set_secret",
            json!({ "id": id, "client_secret": CLIENT_SECRET }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);

    t.stop().await;
}

#[tokio::test]
async fn a_provider_can_change_its_keys() {
    let t = start().await;
    let idp = Idp::start().await;
    provider(&t, &idp, "idp", json!({})).await;
    user(&t, "alice@example.com").await;

    // The keys are read once and remembered.
    for _ in 0..2 {
        let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
        assert_eq!(landing.error(), None, "{}", landing.url);
    }
    assert_eq!(idp.key_set_reads(), 1);

    // The provider replaces its key, here with one of another kind. A
    // token signed with the new key is not known, so the keys are read
    // again, and the sign-in goes through.
    let old = idp.key();
    idp.rotate(SigningKey::rsa("key-2"));
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    assert_eq!(idp.key_set_reads(), 2);
    let (status, session) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::OK, "{session}");

    // The key that was withdrawn no longer signs anybody in.
    let landing = sign_in_with(
        &t,
        &idp,
        "idp",
        "alice@example.com",
        |_| {},
        |c| old.sign(c),
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("invalid_token"));
    assert!(last_refusal(&t).await.contains("does not publish: key-1"));

    // A key the provider never published. The keys were read again a
    // moment ago, so forged tokens do not make this server ask again and
    // again.
    let forger = SigningKey::ec("key-of-a-forger");
    for _ in 0..3 {
        let landing = sign_in_with(
            &t,
            &idp,
            "idp",
            "alice@example.com",
            |_| {},
            |c| forger.sign(c),
        )
        .await;
        assert_eq!(landing.error().as_deref(), Some("invalid_token"));
    }
    assert!(last_refusal(&t).await.contains("key-of-a-forger"));
    assert_eq!(idp.key_set_reads(), 2);

    // The current key still works after all that.
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    assert_eq!(landing.error(), None);

    t.stop().await;
}

#[tokio::test]
async fn the_client_secret_is_never_shown_or_recorded() {
    let t = start().await;
    let idp = Idp::start().await;
    let c = t.company("ACME", "USD").await;
    const FIRST: &str = "first-secret-4f9a1c";
    const SECOND: &str = "second-secret-77b0e2";

    // Created with an idempotency key, which for other commands stores
    // the output for a retry.
    let input = json!({
        "name": "Company login", "slug": "idp", "issuer": idp.issuer,
        "client_id": CLIENT_ID, "client_secret": FIRST, "allowed_domains": ["*"],
    });
    let (status, created) = t
        .send(
            "POST",
            "/api/commands/sso_provider.create",
            Some(&t.admin),
            Some(input.clone()),
            &[("idempotency-key", "provider-1")],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    // A create that fails is audited too, and its input holds the secret.
    let (status, _) = t.command(&t.admin, "sso_provider.create", input).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, replaced) = t
        .command(
            &t.admin,
            "sso_provider.set_secret",
            json!({ "id": id, "client_secret": SECOND }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    let (status, updated) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": id, "name": "Company sign-in", "allowed_domains": ["example.com"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], "Company sign-in");
    assert_eq!(updated["allowed_domains"], json!(["example.com"]));

    // The secret in use is the second one, kept where sign-in can read it.
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM sso_provider_secret WHERE client_secret = '{SECOND}'"
        ))
        .await,
        1
    );

    // Everything a caller can read.
    let (status, list) = t.get(&t.admin, "/api/entities/sso_provider").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["slug"], "idp");
    let (status, one) = t
        .get(&t.admin, &format!("/api/entities/sso_provider/{id}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{one}");
    let (status, listed) = t.command(&t.admin, "sso_provider.list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["configured"], true);
    assert_eq!(
        listed["items"][0]["redirect_uri"],
        format!("{PUBLIC_URL}/api/auth/sso/idp/callback")
    );
    let (_, generic) = t
        .command(&t.admin, "entity.list", json!({ "entity": "sso_provider" }))
        .await;
    let (_, history) = t
        .command(
            &t.admin,
            "audit.history",
            json!({ "resource": "sso_provider", "id": id }),
        )
        .await;
    assert!(history["total"].as_i64().unwrap() >= 3, "{history}");
    let (_, meta) = t.get(&t.admin, "/api/meta?module=auth").await;
    let (_, _, public) = call(&t, "GET", "/api/auth/sso", None, None).await;
    let (_, signins) = t.command(&t.admin, "auth.events", json!({})).await;
    for (what, shown) in [
        ("created", &created),
        ("replaced", &replaced),
        ("updated", &updated),
        ("entity list", &list),
        ("entity read", &one),
        ("sso_provider.list", &listed),
        ("entity.list", &generic),
        ("audit.history", &history),
        ("public list", &public),
        ("auth.events", &signins),
    ] {
        let text = shown.to_string();
        assert!(
            !text.contains(FIRST) && !text.contains(SECOND),
            "{what}: {text}"
        );
        assert!(!text.contains("client_secret"), "{what}: {text}");
    }
    // The entity has no such field to ask for, filter by or sort by.
    assert!(!meta["entities"].to_string().contains("secret"));
    let (status, _) = t
        .get(&t.admin, "/api/entities/sso_provider?client_secret=x")
        .await;
    assert_ne!(status, StatusCode::OK);
    // And it cannot be written past the commands.
    let (status, _) = t
        .post(
            &t.admin,
            "/api/entities/sso_provider",
            json!({ "name": "x" }),
        )
        .await;
    assert_ne!(status, StatusCode::CREATED);
    let (status, _) = t
        .patch(
            &t.admin,
            &format!("/api/entities/sso_provider/{id}"),
            json!({ "issuer": "https://evil.example" }),
        )
        .await;
    assert_ne!(status, StatusCode::OK);

    // Everything the server keeps about what was done.
    for table in [
        "audit_log",
        "outbox_event",
        "idempotency_key",
        "auth_event",
        "sso_provider",
    ] {
        assert_eq!(
            t.count(&format!(
                "SELECT count(*) FROM {table} x
                 WHERE to_jsonb(x)::text LIKE '%{FIRST}%' OR to_jsonb(x)::text LIKE '%{SECOND}%'"
            ))
            .await,
            0,
            "{table} holds a client secret"
        );
    }
    // The commands are on record all the same, without their input.
    assert_eq!(
        t.count(
            "SELECT count(*) FROM audit_log
             WHERE command IN ('sso_provider.create', 'sso_provider.set_secret') AND input IS NULL"
        )
        .await,
        3
    );
    // What an update changed is on record: it holds no secret.
    assert_eq!(
        t.count(
            "SELECT count(*) FROM audit_log
             WHERE command = 'sso_provider.update' AND input->>'name' = 'Company sign-in'"
        )
        .await,
        1
    );

    // Providers are managed by those who may, for all companies.
    let clerk = t
        .user_with_role(
            "clerk@example.com",
            &[
                "sso_provider:read",
                "sso_provider:create",
                "sso_provider:update",
            ],
            &c.id,
        )
        .await;
    for (command, input) in [
        ("sso_provider.list", json!({})),
        (
            "sso_provider.set_secret",
            json!({ "id": id, "client_secret": "taken-over" }),
        ),
        (
            "sso_provider.update",
            json!({ "id": id, "issuer": idp.issuer }),
        ),
        ("sso_provider.delete", json!({ "id": id })),
    ] {
        let (status, body) = t.command(&clerk, command, input).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{command}: {body}");
    }

    // Deleting a provider takes its secret along.
    let (status, body) = t
        .command(&t.admin, "sso_provider.delete", json!({ "id": id }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(t.count("SELECT count(*) FROM sso_provider_secret").await, 0);
    let (status, _) = t
        .command(&t.admin, "sso_provider.delete", json!({ "id": id }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    t.stop().await;
}

/// The provider's settings, as `sso_provider.list` shows them.
async fn listed(t: &TestApp, slug: &str) -> Value {
    let (status, list) = t.command(&t.admin, "sso_provider.list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["slug"] == slug)
        .cloned()
        .unwrap()
}

#[tokio::test]
async fn pointing_a_provider_elsewhere_hands_over_neither_secret_nor_users() {
    let t = start().await;
    let idp = Idp::start().await;
    // A provider run by somebody who wants to sign in as others.
    let evil = Idp::start().await;
    let c = t.company("ACME", "USD").await;
    let created = provider(&t, &idp, "idp", json!({})).await;
    let id = created["id"].clone();
    assert_eq!(created["has_secret"], true);
    let alice = user(&t, "alice@example.com").await;
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    assert_eq!(t.count("SELECT count(*) FROM sso_identity").await, 1);

    // Without sso_provider:manage for all companies, nothing about a
    // provider can be changed: not with the permissions that used to be
    // enough, and not with manage for one company.
    let (old_style, _) = t
        .user_with_rules(
            "old-style@example.com",
            json!([
                "sso_provider:read",
                "sso_provider:create",
                "sso_provider:update"
            ]),
            None,
        )
        .await;
    let one_company = t
        .user_with_role(
            "one-company@example.com",
            &["sso_provider:manage", "sso_provider:read"],
            &c.id,
        )
        .await;
    for token in [&old_style, &one_company] {
        for (command, input) in [
            (
                "sso_provider.update",
                json!({ "id": id, "issuer": evil.issuer }),
            ),
            (
                "sso_provider.update",
                json!({ "id": id, "client_id": "another-client" }),
            ),
            (
                "sso_provider.update",
                json!({ "id": id, "allowed_domains": ["*"] }),
            ),
            (
                "sso_provider.create",
                json!({ "name": "Evil", "slug": "evil", "issuer": evil.issuer,
                        "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
                        "allowed_domains": ["example.com"] }),
            ),
            (
                "sso_identity.set",
                json!({ "user_id": alice, "provider_id": id, "subject": "x" }),
            ),
        ] {
            let (status, body) = t.command(token, command, input).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{command}: {body}");
        }
    }

    // Re-pointing and activating in one go is refused, and changes nothing.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": id, "issuer": evil.issuer, "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(listed(&t, "idp").await["issuer"], idp.issuer.as_str());
    assert_eq!(t.count("SELECT count(*) FROM sso_provider_secret").await, 1);

    // A sign-in on its way when the provider is re-pointed.
    let on_its_way = begin(&t, "idp").await;

    // Re-pointed: the secret and the bindings are gone, and the provider is
    // inactive.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": id, "issuer": evil.issuer }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["issuer"], evil.issuer.as_str());
    assert_eq!(body["active"], false);
    assert_eq!(body["has_secret"], false);
    assert_eq!(t.count("SELECT count(*) FROM sso_provider_secret").await, 0);
    assert_eq!(t.count("SELECT count(*) FROM sso_identity").await, 0);
    assert_eq!(listed(&t, "idp").await["has_secret"], false);

    // It cannot be activated without a secret, neither by command nor
    // behind the commands' back.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": id, "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        sqlx::query("UPDATE sso_provider SET active = true")
            .execute(&t.pool)
            .await
            .is_err()
    );

    // Nobody can sign in through it, and nothing reaches the new issuer.
    let (status, _, _) = call(&t, "GET", "/api/auth/sso/idp/start", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let code = evil.authorize(
        &on_its_way,
        evil.key()
            .sign(&evil.claims(&on_its_way, "alice@example.com")),
    );
    let landing = come_back(&t, &on_its_way, &code, Some(&on_its_way.cookie)).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));
    assert_eq!(evil.token_requests(), 0, "the old secret went nowhere");
    assert_eq!(idp.token_requests(), 1);

    // The change is on record: who made it and what changed, never a
    // secret.
    let changes = events(&t, json!({ "kind": "sso_provider" })).await;
    assert_eq!(changes[0]["email"], common::ADMIN_EMAIL);
    assert_eq!(changes[0]["provider"], "idp");
    let what = changes[0]["reason"].as_str().unwrap();
    assert!(what.starts_with("changed issuer, active;"), "{what}");
    assert!(what.contains("client secret and 1 bound identities were discarded"));
    assert_eq!(
        changes.last().unwrap()["reason"]
            .as_str()
            .unwrap()
            .split(',')
            .next(),
        Some("created")
    );
    let record = Value::from(events(&t, json!({})).await).to_string();
    assert!(!record.contains(CLIENT_SECRET));

    // Changing the client id does the same.
    let other = provider(&t, &idp, "other", json!({})).await;
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": other["id"], "client_id": "another-client" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["has_secret"], false);
    assert_eq!(body["active"], false);

    // Whoever may manage providers may give the re-pointed one a secret
    // and switch it on. It then signs in, by email, users of the domains it
    // names who are not bound elsewhere: that is why the permission is held
    // as closely as administrator rights. But never an administrator.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.set_secret",
            json!({ "id": id, "client_secret": CLIENT_SECRET }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["active"], false);
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": id, "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let landing = sign_in(&t, &evil, "idp", common::ADMIN_EMAIL).await;
    assert_eq!(landing.error().as_deref(), Some("admin_not_allowed"));
    assert!(
        last_refusal(&t)
            .await
            .contains("not trusted for administrators")
    );
    assert_eq!(
        t.count("SELECT count(*) FROM sso_login_code").await,
        0,
        "no administrator was signed in"
    );

    t.stop().await;
}

#[tokio::test]
async fn a_user_is_bound_to_the_identity_of_their_first_sign_in() {
    let t = start().await;
    let idp = Idp::start().await;
    let first = provider(&t, &idp, "first", json!({})).await;
    provider(&t, &idp, "second", json!({})).await;
    let anywhere = provider(&t, &idp, "anywhere", json!({ "allowed_domains": ["*"] })).await;
    let alice = user(&t, "alice@example.com").await;
    let key = idp.key();

    // Alice's first sign-in binds her to the provider and the subject.
    let landing = sign_in(&t, &idp, "first", "alice@example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    let (status, bindings) = t
        .command(&t.admin, "sso_identity.list", json!({ "user_id": alice }))
        .await;
    assert_eq!(status, StatusCode::OK, "{bindings}");
    assert_eq!(
        bindings["items"],
        json!([{
            "user_id": alice, "email": "alice@example.com", "provider_id": first["id"],
            "provider": "first", "subject": "subject-of-alice@example.com",
            "created_at": bindings["items"][0]["created_at"],
        }])
    );

    // Another provider cannot sign her in, although it names her domain.
    let landing = sign_in(&t, &idp, "second", "alice@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("not_bound"));
    // Nor can the same provider under another subject: an address that
    // passed to somebody else at the provider.
    let landing = sign_in_with(
        &t,
        &idp,
        "first",
        "alice@example.com",
        |claims| claims["sub"] = json!("somebody-else"),
        |claims| key.sign(claims),
    )
    .await;
    assert_eq!(landing.error().as_deref(), Some("not_bound"));
    // The bound identity is Alice whatever address it states now.
    let landing = sign_in_with(
        &t,
        &idp,
        "first",
        "alice.renamed@example.com",
        |claims| claims["sub"] = json!("subject-of-alice@example.com"),
        |claims| key.sign(claims),
    )
    .await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    let (_, session) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    let (_, me) = t
        .get(session["token"].as_str().unwrap(), "/api/auth/me")
        .await;
    assert_eq!(me["user_id"], alice.as_str());

    // A provider for any domain binds nobody by address: Bob is not bound,
    // so it cannot sign him in; and it creates nobody.
    let bob = user(&t, "bob@example.com").await;
    let landing = sign_in(&t, &idp, "anywhere", "bob@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("not_bound"));
    let landing = sign_in(&t, &idp, "anywhere", "nobody@other.org").await;
    assert_eq!(landing.error().as_deref(), Some("no_account"));
    // Unless an administrator binds him to it, which is on record.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_identity.set",
            json!({ "user_id": bob, "provider_id": anywhere["id"],
                    "subject": "subject-of-bob@other.org" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["provider"], "anywhere");
    let landing = sign_in(&t, &idp, "anywhere", "bob@other.org").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    let changes = events(
        &t,
        json!({ "kind": "sso_identity", "provider": "anywhere" }),
    )
    .await;
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["email"], common::ADMIN_EMAIL);
    assert_eq!(
        changes[0]["reason"],
        "bound bob@example.com to subject subject-of-bob@other.org, was not bound"
    );
    // An identity belongs to one user.
    let (status, body) = t
        .command(
            &t.admin,
            "sso_identity.set",
            json!({ "user_id": alice, "provider_id": anywhere["id"],
                    "subject": "subject-of-bob@other.org" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Undoing Alice's binding lets the next allowed sign-in bind her again.
    let (status, body) = t
        .command(&t.admin, "sso_identity.delete", json!({ "user_id": alice }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let landing = sign_in(&t, &idp, "second", "alice@example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    let landing = sign_in(&t, &idp, "first", "alice@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("not_bound"));
    let changes = events(&t, json!({ "kind": "sso_identity", "provider": "first" })).await;
    assert_eq!(
        changes[0]["reason"],
        "unbound alice@example.com from subject subject-of-alice@example.com"
    );

    // An administrator is never signed in by address through a provider
    // that is not trusted for administrators.
    let landing = sign_in(&t, &idp, "first", common::ADMIN_EMAIL).await;
    assert_eq!(landing.error().as_deref(), Some("admin_not_allowed"));
    assert_eq!(
        t.count(&format!(
            "SELECT count(*) FROM sso_identity i JOIN app_user u ON u.id = i.user_id
             WHERE u.email = '{}'",
            common::ADMIN_EMAIL
        ))
        .await,
        0
    );
    // Only an administrator may trust a provider for administrators, bind
    // an administrator, or touch a trusted provider.
    let (manager, _) = t
        .user_with_rules(
            "manager@example.com",
            json!(["sso_provider:manage", "sso_provider:read"]),
            None,
        )
        .await;
    let (status, body) = t
        .command(
            &manager,
            "sso_provider.create",
            json!({ "name": "Admins", "slug": "admins", "issuer": idp.issuer,
                    "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
                    "allowed_domains": ["example.com"], "trusted_for_admins": true }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = t
        .command(
            &manager,
            "sso_provider.update",
            json!({ "id": first["id"], "trusted_for_admins": true }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let admin_id: Uuid = sqlx::query_scalar("SELECT id FROM app_user WHERE email = $1")
        .bind(common::ADMIN_EMAIL)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let (status, body) = t
        .command(
            &manager,
            "sso_identity.set",
            json!({ "user_id": admin_id, "provider_id": first["id"], "subject": "mine" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The manager may bind a user who is not an administrator.
    let (status, body) = t
        .command(&manager, "sso_identity.delete", json!({ "user_id": bob }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let trusted = provider(&t, &idp, "admins", json!({ "trusted_for_admins": true })).await;
    let landing = sign_in(&t, &idp, "admins", common::ADMIN_EMAIL).await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    for (command, input) in [
        (
            "sso_provider.update",
            json!({ "id": trusted["id"], "name": "Renamed" }),
        ),
        (
            "sso_provider.set_secret",
            json!({ "id": trusted["id"], "client_secret": "x" }),
        ),
        ("sso_provider.delete", json!({ "id": trusted["id"] })),
        ("sso_identity.delete", json!({ "user_id": admin_id })),
    ] {
        let (status, body) = t.command(&manager, command, input).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{command}: {body}");
    }

    // A provider saved before domains were required has none, and signs
    // nobody in until it is given some.
    sqlx::query("UPDATE sso_provider SET allowed_domains = '{}' WHERE slug = 'second'")
        .execute(&t.pool)
        .await
        .unwrap();
    let landing = sign_in(&t, &idp, "second", "alice@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("domain_not_allowed"));
    let second = listed(&t, "second").await;
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": second["id"], "name": "Second" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "allowed_domains");

    t.stop().await;
}

#[tokio::test]
async fn granting_a_role_through_a_provider_takes_the_permission_to_grant_roles() {
    let t = start().await;
    let idp = Idp::start().await;
    let role = t
        .create(
            "role",
            json!({ "name": "Newcomer", "permissions": ["account:read"] }),
        )
        .await;
    let with_role = provider(
        &t,
        &idp,
        "open",
        json!({ "auto_create_users": true, "default_role_id": role }),
    )
    .await;
    let id = &with_role["id"];
    let (manager, _) = t
        .user_with_rules(
            "manager@example.com",
            json!(["sso_provider:manage", "sso_provider:read"]),
            None,
        )
        .await;

    // Who the provider lets in decides who gets the role: every change to
    // that takes user:assign_role, not only one to the role.
    for change in [
        json!({ "allowed_domains": ["example.com", "example.org"] }),
        json!({ "auto_create_users": false }),
        json!({ "require_verified_email": false }),
        json!({ "default_role_id": null }),
        json!({ "issuer": format!("{}/v2", idp.issuer) }),
    ] {
        let mut input = change.clone();
        input["id"] = id.clone();
        let (status, body) = t.command(&manager, "sso_provider.update", input).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{change}: {body}");
    }
    // What does not change who gets in does not.
    for change in [json!({ "name": "Open door" }), json!({ "active": false })] {
        let mut input = change.clone();
        input["id"] = id.clone();
        let (status, body) = t.command(&manager, "sso_provider.update", input).await;
        assert_eq!(status, StatusCode::OK, "{change}: {body}");
    }
    // Switching it back on does.
    let (status, body) = t
        .command(
            &manager,
            "sso_provider.update",
            json!({ "id": id, "active": true }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Nothing was changed by the refused attempts.
    let now = listed(&t, "open").await;
    assert_eq!(now["allowed_domains"], json!(["example.com"]));
    assert_eq!(now["auto_create_users"], true);
    assert_eq!(now["default_role_id"], role.as_str());
    assert_eq!(now["active"], false);

    t.stop().await;
}

#[tokio::test]
async fn over_https_the_browser_cookie_is_a_host_cookie_of_one_exact_name() {
    const HTTPS_URL: &str = "https://erp.example.com";
    let t = start_at(HTTPS_URL).await;
    let idp = Idp::start().await;
    provider(&t, &idp, "idp", json!({})).await;
    user(&t, "alice@example.com").await;

    let started = begin(&t, "idp").await;
    assert_eq!(
        started.redirect_uri,
        format!("{HTTPS_URL}/api/auth/sso/idp/callback")
    );
    let (name, value) = started.cookie.split_once('=').unwrap();
    assert!(name.starts_with("__Host-erpcraft_sso_"), "{name}");
    let (_, headers, _) = call(&t, "GET", "/api/auth/sso/idp/start", None, None).await;
    let set_cookie = headers["set-cookie"].to_str().unwrap();
    for attribute in ["; Secure", "; Path=/;", "; HttpOnly", "; SameSite=Lax"] {
        assert!(set_cookie.contains(attribute), "{set_cookie}");
    }
    assert!(!set_cookie.contains("Domain"), "{set_cookie}");

    // The right value under a name a sibling subdomain can set, without
    // the prefix, or under the name of another sign-in, is not the cookie.
    let unprefixed = format!("{}={value}", name.trim_start_matches("__Host-"));
    let code = idp.authorize(
        &started,
        idp.key().sign(&idp.claims(&started, "alice@example.com")),
    );
    let landing = come_back(&t, &started, &code, Some(&unprefixed)).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));
    let started = begin(&t, "idp").await;
    let (_, value) = started.cookie.split_once('=').unwrap();
    let another = format!("__Host-erpcraft_sso_0000000000000000={value}");
    let code = idp.authorize(
        &started,
        idp.key().sign(&idp.claims(&started, "alice@example.com")),
    );
    let landing = come_back(&t, &started, &code, Some(&another)).await;
    assert_eq!(landing.error().as_deref(), Some("invalid_state"));
    assert_eq!(idp.token_requests(), 0);

    // The exact cookie finishes the sign-in, and only it collects the token.
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    assert_eq!(landing.error(), None, "{}", landing.url);
    assert!(landing.url.as_str().starts_with(HTTPS_URL));
    let (cookie_name, cookie_value) = landing.cookie.split_once('=').unwrap();
    let planted = format!(
        "{}={cookie_value}",
        cookie_name.trim_start_matches("__Host-")
    );
    let (status, _) = exchange(&t, &landing.code(), Some(&planted)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let landing = sign_in(&t, &idp, "idp", "alice@example.com").await;
    let (status, session) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::OK, "{session}");

    t.stop().await;
}

#[tokio::test]
async fn a_sign_in_from_an_untrusted_provider_never_acts_as_an_administrator() {
    let t = start().await;
    let idp = Idp::start().await;
    provider(&t, &idp, "idp", json!({})).await;
    let sam = user(&t, "sam@example.com").await;

    // Sam signs in through the provider, which is not trusted for
    // administrators, and makes a long-lived token from that session.
    let landing = sign_in(&t, &idp, "idp", "sam@example.com").await;
    let (status, session) = exchange(&t, &landing.code(), Some(&landing.cookie)).await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let session = session["token"].as_str().unwrap().to_owned();
    let (status, body) = t
        .command(
            &session,
            "auth.create_token",
            json!({ "name": "agent", "expires_in_days": 3650 }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["field"], "expires_in_days");
    let (status, derived) = t
        .command(
            &session,
            "auth.create_token",
            json!({ "name": "agent", "expires_in_days": 365 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{derived}");
    let derived = derived["token"].as_str().unwrap().to_owned();
    assert_eq!(
        t.count(
            "SELECT count(*) FROM api_token WHERE name = 'agent' AND signed_in_with = 'sso:idp'"
        )
        .await,
        1,
        "the token carries its origin on"
    );
    // A login code finished before the promotion, collected after it.
    let waiting = sign_in(&t, &idp, "idp", "sam@example.com").await;

    // Sam is made an administrator.
    let (status, body) = t
        .command(
            &t.admin,
            "user.update",
            json!({ "id": sam, "is_admin": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Neither the session, the token made from it, nor the waiting login
    // code acts as an administrator: they are refused.
    for token in [&session, &derived] {
        let (status, _) = t.get(token, "/api/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = t.command(token, "user.list", json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, _) = exchange(&t, &waiting.code(), Some(&waiting.cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        last_refusal(&t)
            .await
            .contains("not trusted for administrators")
    );
    // A new sign-in through it is refused too, as before.
    let landing = sign_in(&t, &idp, "idp", "sam@example.com").await;
    assert_eq!(landing.error().as_deref(), Some("admin_not_allowed"));
    // With a password, Sam is an administrator.
    let (status, _, login) = call(
        &t,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "email": "sam@example.com", "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let (status, me) = t
        .get(login["token"].as_str().unwrap(), "/api/auth/me")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["is_admin"], true);

    // Once the provider is trusted for administrators, the old session
    // works again, as an administrator.
    let (_, list) = t.command(&t.admin, "sso_provider.list", json!({})).await;
    let id = list["items"][0]["id"].clone();
    let (status, body) = t
        .command(
            &t.admin,
            "sso_provider.update",
            json!({ "id": id, "trusted_for_admins": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = t.get(&session, "/api/auth/me").await;
    assert_eq!(status, StatusCode::OK);
    t.stop().await;
}
