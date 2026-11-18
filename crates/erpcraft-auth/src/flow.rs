//! Signing in through a provider: the authorization code flow with PKCE.
//!
//! 1. `GET /api/auth/sso/{slug}/start` notes a login attempt and sends the
//!    browser to the provider.
//! 2. The provider sends it back to `GET /api/auth/sso/{slug}/callback`.
//!    The code is exchanged, the ID token checked, the user found or
//!    created, and the browser sent on to where it wanted to go, with a
//!    login code in the query.
//! 3. The page there posts the code to `POST /api/auth/sso/exchange` and
//!    receives the token, as from a password sign-in.
//!
//! The token never appears in a URL. The login code that does is good
//! once, for a minute, and only in the browser that started the sign-in.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use erpcraft_api::{ApiError, ApiResult, parse_body};
use erpcraft_kernel::authz;
use erpcraft_kernel::{Error, Kernel, PgPool, Result, new_id};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

use crate::events::{self, AuthEvent};
use crate::oidc::{Identity, Oidc, Refusal};
use crate::sso::{COLUMNS, Provider, SsoConfig};
use crate::{LOGIN_TOKEN_DAYS, hash_token, issue_token};

/// How long somebody has to sign in at the provider.
const ATTEMPT_MINUTES: i32 = 10;
/// How long the page has to collect its token.
const CODE_SECONDS: i32 = 60;
/// Where the browser is sent when a sign-in is refused, with `sso_error`.
const SIGN_IN_PATH: &str = "/sign-in";
/// The browser's cookie is this, followed by the attempt's own id. Over
/// https the name starts with `__Host-`, which a browser accepts only
/// from this host itself, over https, for the path `/` and without a
/// domain: a sibling subdomain cannot plant it. Plain http is allowed
/// only on this machine, where the prefix cannot be used.
const COOKIE_NAME: &str = "erpcraft_sso_";
const HOST_PREFIX: &str = "__Host-";
const MAX_RETURN_TO: usize = 2000;
/// `sub` is at most 255 characters by OpenID Connect.
const MAX_SUBJECT: usize = 255;

#[derive(Clone)]
struct Sso {
    kernel: Kernel,
    config: SsoConfig,
    /// `None` if the HTTP client could not be set up.
    oidc: Option<Arc<Oidc>>,
}

pub(crate) fn routes(kernel: Kernel, config: SsoConfig) -> Router {
    let oidc = match Oidc::new(config.allow_insecure()) {
        Ok(oidc) => Some(Arc::new(oidc)),
        Err(error) => {
            tracing::error!(%error, "single sign-on is off: the HTTP client could not be set up");
            None
        }
    };
    Router::new()
        .route("/api/auth/sso", get(providers))
        .route("/api/auth/sso/exchange", post(exchange))
        .route("/api/auth/sso/{slug}/start", get(start))
        .route("/api/auth/sso/{slug}/callback", get(callback))
        .with_state(Sso {
            kernel,
            config,
            oidc,
        })
}

// ------------------------------------------------------------------ helpers

/// 244 random bits from the system's generator, as 64 characters that are
/// safe in a URL, a cookie and a PKCE verifier.
fn random() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Compare two hashes without telling, through the time taken, where they
/// differ.
fn same(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// `return_to` as a path on this server, or `None` if it is anything else.
/// An absolute URL, or `//host`, would send a signed-in browser, with a
/// login code, to another site.
fn local_path(text: &str) -> Option<String> {
    let ok = text.starts_with('/')
        && !text.starts_with("//")
        && !text.contains('\\')
        && !text.chars().any(char::is_control)
        && text.len() <= MAX_RETURN_TO;
    if !ok {
        return None;
    }
    // What a browser makes of it must stay on the same host.
    let base = Url::parse("https://local.invalid/").ok()?;
    let joined = base.join(text).ok()?;
    (joined.scheme() == "https" && joined.host_str() == Some("local.invalid"))
        .then(|| text.to_owned())
}

/// The values of the cookies of a request with exactly this name.
fn cookies_named<'a>(headers: &'a HeaderMap, wanted: &'a str) -> impl Iterator<Item = &'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(move |pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == wanted).then_some(value)
        })
}

/// Whether the request carries the cookie that proves this is the browser
/// that started the sign-in: the one of exactly the name given to it, with
/// the value whose hash was noted.
fn has_browser_cookie(headers: &HeaderMap, name: &str, browser_hash: &str) -> bool {
    cookies_named(headers, name).any(|value| same(&hash_token(value), browser_hash))
}

/// A short id, random, that makes the name of an attempt's cookie, so
/// that sign-ins in two tabs do not overwrite each other's.
fn cookie_id() -> String {
    random()[..16].to_owned()
}

impl Sso {
    fn base(&self) -> Result<String> {
        self.config.public_base().ok_or_else(|| {
            Error::state(
                "single sign-on is not set up: the server has no public URL (ERPCRAFT_PUBLIC_URL)",
            )
        })
    }

    /// The name of the cookie of the attempt with this id.
    fn cookie_name(&self, id: &str) -> String {
        cookie_name(self.config.is_https(), id)
    }

    fn cookie(&self, id: &str, value: &str, seconds: i32) -> HeaderValue {
        let header = set_cookie(self.config.is_https(), id, value, seconds);
        HeaderValue::from_str(&header).unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    /// Send the browser to the sign-in page, saying in one word why the
    /// sign-in did not go through.
    fn refused(&self, base: &str, code: &str, return_to: Option<&str>) -> Response {
        let mut location = format!("{base}{SIGN_IN_PATH}?sso_error={code}");
        if let Some(return_to) = return_to.filter(|path| *path != "/") {
            let encoded: String =
                url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
            location.push_str(&format!("&return_to={encoded}"));
        }
        redirect(&location, None)
    }
}

fn cookie_name(https: bool, id: &str) -> String {
    let prefix = if https { HOST_PREFIX } else { "" };
    format!("{prefix}{COOKIE_NAME}{id}")
}

/// The `Set-Cookie` header of an attempt's cookie. `Path=/` and no
/// `Domain`, as `__Host-` requires; Lax, not Strict, because the
/// provider's redirect back is a navigation from another site and must
/// carry the cookie.
fn set_cookie(https: bool, id: &str, value: &str, seconds: i32) -> String {
    let name = cookie_name(https, id);
    let secure = if https { "; Secure" } else { "" };
    format!("{name}={value}; Path=/; Max-Age={seconds}; HttpOnly; SameSite=Lax{secure}")
}

fn redirect(location: &str, cookie: Option<HeaderValue>) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    let headers = response.headers_mut();
    if let Ok(location) = HeaderValue::from_str(location) {
        headers.insert(LOCATION, location);
    }
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(cookie) = cookie {
        headers.insert(SET_COOKIE, cookie);
    }
    response
}

async fn provider_by(
    pool: &PgPool,
    column: &'static str,
    slug_or_id: &str,
) -> Result<Option<Provider>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM sso_provider WHERE {column}::text = $1"
    ))
    .bind(slug_or_id)
    .fetch_optional(pool)
    .await?)
}

// ---------------------------------------------------------------- providers

/// The providers to offer on the sign-in page. Needs no token.
async fn providers(State(sso): State<Sso>) -> ApiResult {
    // Without a public URL nobody can sign in through a provider, so
    // there is nothing to offer.
    let providers: Vec<(String, String)> = if sso.config.public_base().is_some() {
        sqlx::query_as("SELECT slug, name FROM sso_provider WHERE active ORDER BY name, slug")
            .fetch_all(sso.kernel.pool())
            .await
            .map_err(Error::from)?
    } else {
        Vec::new()
    };
    let providers: Vec<_> = providers
        .into_iter()
        .map(|(slug, name)| json!({ "slug": slug, "name": name }))
        .collect();
    erpcraft_api::ok(json!({ "providers": providers }))
}

// -------------------------------------------------------------------- start

#[derive(Deserialize)]
struct StartQuery {
    return_to: Option<String>,
}

async fn start(
    State(sso): State<Sso>,
    Path(slug): Path<String>,
    Query(query): Query<StartQuery>,
) -> Result<Response, ApiError> {
    let base = sso.base()?;
    let return_to = match query.return_to.as_deref() {
        None | Some("") => "/".to_owned(),
        Some(text) => local_path(text).ok_or_else(|| {
            Error::field(
                "return_to",
                "must be a path on this server, such as '/c/acme'",
            )
        })?,
    };
    let pool = sso.kernel.pool();
    let provider = provider_by(pool, "slug", &slug)
        .await?
        .filter(|provider| provider.active)
        .ok_or_else(|| Error::not_found("sso_provider"))?;
    let redirect_uri = sso.config.redirect_uri(&provider.slug).unwrap_or_default();

    let discovery = match &sso.oidc {
        Some(oidc) => oidc.discovery(&provider.issuer).await,
        None => Err(Refusal::new(
            "provider_unavailable",
            "the HTTP client could not be set up",
        )),
    };
    let discovery = match discovery {
        Ok(discovery) => discovery,
        Err(refusal) => {
            events::record(
                pool,
                AuthEvent {
                    kind: "sso",
                    provider: Some(&provider.slug),
                    refused: Some(&refusal.reason),
                    ..AuthEvent::default()
                },
            )
            .await;
            return Ok(sso.refused(&base, refusal.code, Some(&return_to)));
        }
    };

    let (state, nonce, verifier, browser) = (random(), random(), random(), random());
    let cookie_id = cookie_id();
    // Attempts nobody came back for go when the next one starts.
    sqlx::query("DELETE FROM sso_login_attempt WHERE expires_at < now()")
        .execute(pool)
        .await
        .map_err(Error::from)?;
    sqlx::query(
        "INSERT INTO sso_login_attempt
            (state_hash, provider_id, nonce, pkce_verifier, browser_hash, cookie_id, return_to,
             expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(mins => $8))",
    )
    .bind(hash_token(&state))
    .bind(provider.id)
    .bind(&nonce)
    .bind(&verifier)
    .bind(hash_token(&browser))
    .bind(&cookie_id)
    .bind(&return_to)
    .bind(ATTEMPT_MINUTES)
    .execute(pool)
    .await
    .map_err(Error::from)?;

    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut location = discovery.authorization_endpoint.clone();
    location
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &provider.scopes)
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(redirect(
        location.as_str(),
        Some(sso.cookie(&cookie_id, &browser, ATTEMPT_MINUTES * 60)),
    ))
}

// ----------------------------------------------------------------- callback

/// What is known about a sign-in while it is being worked out, for the
/// record of it.
#[derive(Default)]
struct Known {
    provider: Option<String>,
    email: Option<String>,
    subject: Option<String>,
    user_id: Option<Uuid>,
    return_to: Option<String>,
}

/// A sign-in that went through.
struct SignedIn {
    return_to: String,
    login_code: String,
    /// The id of the browser's cookie, and its value.
    cookie: (String, String),
}

#[derive(sqlx::FromRow)]
struct Attempt {
    provider_id: Uuid,
    nonce: String,
    pkce_verifier: String,
    browser_hash: String,
    /// `None` for an attempt from before cookies had one exact name.
    cookie_id: Option<String>,
    return_to: String,
    live: bool,
}

async fn callback(
    State(sso): State<Sso>,
    Path(slug): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let base = sso.base()?;
    let pool = sso.kernel.pool();
    let mut known = Known::default();
    let outcome = finish(&sso, &slug, &query, &headers, &mut known).await;
    let refusal = match outcome {
        Ok(Ok(signed_in)) => {
            events::record(
                pool,
                AuthEvent {
                    kind: "sso",
                    email: known.email.as_deref(),
                    provider: known.provider.as_deref(),
                    subject: known.subject.as_deref(),
                    user_id: known.user_id,
                    refused: None,
                },
            )
            .await;
            let mut location =
                Url::parse(&format!("{base}{}", signed_in.return_to)).map_err(Error::internal)?;
            location
                .query_pairs_mut()
                .append_pair("sso_code", &signed_in.login_code);
            let (id, value) = &signed_in.cookie;
            // The cookie is needed once more, to collect the token.
            return Ok(redirect(
                location.as_str(),
                Some(sso.cookie(id, value, CODE_SECONDS * 2)),
            ));
        }
        Ok(Err(refusal)) => refusal,
        Err(error) => {
            tracing::error!(%error, "a single sign-on could not be completed");
            Refusal::new("failed", "an internal error; see the server log")
        }
    };
    events::record(
        pool,
        AuthEvent {
            kind: "sso",
            email: known.email.as_deref(),
            provider: known.provider.as_deref(),
            subject: known.subject.as_deref(),
            user_id: known.user_id,
            refused: Some(&refusal.reason),
        },
    )
    .await;
    Ok(sso.refused(&base, refusal.code, known.return_to.as_deref()))
}

/// The outer error is one of this server; the inner one a sign-in that is
/// refused.
async fn finish(
    sso: &Sso,
    slug: &str,
    query: &HashMap<String, String>,
    headers: &HeaderMap,
    known: &mut Known,
) -> Result<std::result::Result<SignedIn, Refusal>> {
    let pool = sso.kernel.pool();
    let refuse = |code: &'static str, reason: &str| Ok(Err(Refusal::new(code, reason)));

    // The state names the attempt. Taking the row out is what makes a
    // state good once: of two requests with the same state, one finds it.
    let Some(state) = query.get("state").filter(|state| !state.is_empty()) else {
        return refuse("invalid_state", "the answer carried no state");
    };
    let attempt: Option<Attempt> = sqlx::query_as(
        "DELETE FROM sso_login_attempt WHERE state_hash = $1
         RETURNING provider_id, nonce, pkce_verifier, browser_hash, cookie_id, return_to,
                   expires_at > now() AS live",
    )
    .bind(hash_token(state))
    .fetch_optional(pool)
    .await?;
    let Some(attempt) = attempt else {
        return refuse("invalid_state", "the state is unknown or was used before");
    };
    known.return_to = Some(attempt.return_to.clone());
    let provider = provider_by(pool, "id", &attempt.provider_id.to_string()).await?;
    let Some(provider) = provider.filter(|p| p.slug == slug) else {
        return refuse(
            "invalid_state",
            "the sign-in was started with another provider",
        );
    };
    known.provider = Some(provider.slug.clone());
    if !attempt.live {
        return refuse("invalid_state", "the sign-in took too long and has expired");
    }
    if !provider.active {
        return refuse("provider_unavailable", "the provider is not active");
    }
    // Only the browser that started the sign-in can finish it. Without
    // this, a link to somebody else's finished sign-in would sign the
    // person who opens it in as that somebody.
    let cookie = attempt.cookie_id.as_deref().and_then(|id| {
        cookies_named(headers, &sso.cookie_name(id))
            .find(|value| same(&hash_token(value), &attempt.browser_hash))
            .map(|value| (id.to_owned(), value.to_owned()))
    });
    let Some(cookie) = cookie else {
        return refuse(
            "invalid_state",
            "the sign-in was not started in this browser",
        );
    };

    if let Some(error) = query.get("error") {
        let description = query
            .get("error_description")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        return Ok(Err(Refusal::new(
            "provider_refused",
            format!("the provider answered {error}{description}"),
        )));
    }
    let Some(code) = query.get("code").filter(|code| !code.is_empty()) else {
        return refuse("provider_refused", "the answer carried no code");
    };
    let Some(oidc) = &sso.oidc else {
        return refuse(
            "provider_unavailable",
            "the HTTP client could not be set up",
        );
    };
    let secret: Option<String> =
        sqlx::query_scalar("SELECT client_secret FROM sso_provider_secret WHERE provider_id = $1")
            .bind(provider.id)
            .fetch_optional(pool)
            .await?;
    let Some(secret) = secret else {
        return refuse("provider_unavailable", "the provider has no client secret");
    };
    let redirect_uri = sso.config.redirect_uri(&provider.slug).unwrap_or_default();

    let identity = async {
        let discovery = oidc.discovery(&provider.issuer).await?;
        let id_token = oidc
            .exchange(
                &discovery,
                &provider.client_id,
                &secret,
                code,
                &redirect_uri,
                &attempt.pkce_verifier,
            )
            .await?;
        oidc.identity(
            &provider.issuer,
            &provider.client_id,
            &discovery,
            &id_token,
            &attempt.nonce,
        )
        .await
    }
    .await;
    let identity = match identity {
        Ok(identity) => identity,
        Err(refusal) => return Ok(Err(refusal)),
    };

    let user_id = match user_for(pool, &provider, &identity, known).await? {
        Ok(user_id) => user_id,
        Err(refusal) => return Ok(Err(refusal)),
    };

    let login_code = random();
    sqlx::query("DELETE FROM sso_login_code WHERE expires_at < now()")
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO sso_login_code
            (code_hash, user_id, provider_id, browser_hash, cookie_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
    )
    .bind(hash_token(&login_code))
    .bind(user_id)
    .bind(provider.id)
    .bind(&attempt.browser_hash)
    .bind(&cookie.0)
    .bind(f64::from(CODE_SECONDS))
    .execute(pool)
    .await?;
    Ok(Ok(SignedIn {
        return_to: attempt.return_to,
        login_code,
        cookie,
    }))
}

/// The user an identity belongs to.
///
/// A user who is bound to an identity (`sso_identity`) is found by it, and
/// only by it: the provider and the subject it states. A user who is not
/// bound yet is found by email address, but only through a provider that
/// names the address's domain, and is bound to this identity from then on.
/// Somebody who is not a user yet is created, if the provider creates
/// users. An administrator signs in only through a provider trusted for
/// administrators.
async fn user_for(
    pool: &PgPool,
    provider: &Provider,
    identity: &Identity,
    known: &mut Known,
) -> Result<std::result::Result<Uuid, Refusal>> {
    let refuse = |code: &'static str, reason: String| Ok(Err(Refusal::new(code, reason)));
    let subject = identity.subject.as_str();
    if subject.is_empty()
        || subject.chars().count() > MAX_SUBJECT
        || subject.chars().any(char::is_control)
    {
        return refuse(
            "invalid_token",
            "the provider stated no usable subject (sub)".into(),
        );
    }
    known.subject = Some(subject.to_owned());
    let email = identity
        .email
        .as_deref()
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty() && email.len() <= 320);
    let Some(email) = email else {
        return refuse(
            "no_email",
            "the provider stated no email address; check that it grants the scope 'email'".into(),
        );
    };
    known.email = Some(email.clone());
    // Users are matched by address at their first sign-in, so an address
    // the provider does not vouch for could be anybody's.
    if provider.require_verified_email && !identity.email_verified {
        return refuse(
            "email_not_verified",
            "the provider does not state that the email address is verified".into(),
        );
    }
    let domain = email.rsplit_once('@').map(|(_, domain)| domain);
    let Some(domain) = domain.filter(|d| !d.is_empty()) else {
        return refuse(
            "no_email",
            "the provider stated no usable email address".into(),
        );
    };
    if !provider.allows(domain) {
        return refuse(
            "domain_not_allowed",
            format!("the domain {domain} may not sign in through this provider"),
        );
    }

    // Two sign-ins of the same person at the same moment: the second finds
    // what the first bound or created.
    match bind(pool, provider, identity, &email, domain, known).await {
        Err(Error::Conflict(_)) => bind(pool, provider, identity, &email, domain, known).await,
        outcome => outcome,
    }
}

#[derive(sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    active: bool,
    is_admin: bool,
    /// Whether the user is bound to an identity, any.
    bound: bool,
}

/// Find the user of an identity whose email domain the provider allows,
/// binding or creating them where that is allowed.
async fn bind(
    pool: &PgPool,
    provider: &Provider,
    identity: &Identity,
    email: &str,
    domain: &str,
    known: &mut Known,
) -> Result<std::result::Result<Uuid, Refusal>> {
    let refuse = |code: &'static str, reason: String| Ok(Err(Refusal::new(code, reason)));
    let admit = |user: &Candidate| -> std::result::Result<Uuid, Refusal> {
        if user.is_admin && !provider.trusted_for_admins {
            return Err(Refusal::new(
                "admin_not_allowed",
                "the user is an administrator, and the provider is not trusted for administrators",
            ));
        }
        if !user.active {
            return Err(Refusal::new("inactive", "the user is inactive"));
        }
        Ok(user.id)
    };

    let mut tx = pool.begin().await?;
    let bound: Option<Candidate> = sqlx::query_as(
        "SELECT u.id, u.active, u.is_admin, true AS bound
         FROM sso_identity i JOIN app_user u ON u.id = i.user_id
         WHERE i.provider_id = $1 AND i.subject = $2",
    )
    .bind(provider.id)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(user) = bound {
        known.user_id = Some(user.id);
        return Ok(admit(&user));
    }

    // Locked, so that a binding made meanwhile by sso_identity.set or by
    // another sign-in is seen.
    let user: Option<Candidate> = sqlx::query_as(
        "SELECT u.id, u.active, u.is_admin,
                EXISTS (SELECT 1 FROM sso_identity i WHERE i.user_id = u.id) AS bound
         FROM app_user u WHERE u.email = $1 FOR UPDATE",
    )
    .bind(email)
    .fetch_optional(&mut *tx)
    .await?;
    let user_id = match user {
        Some(user) => {
            known.user_id = Some(user.id);
            if user.bound {
                return refuse(
                    "not_bound",
                    "the user signs in through another provider or as another identity; an \
                     administrator can change that with sso_identity.set"
                        .into(),
                );
            }
            // Any domain (`*`) lets in the users bound to the provider,
            // never somebody because of an address alone.
            if !provider.names(domain) {
                return refuse(
                    "not_bound",
                    format!(
                        "the user is not bound to this provider, and the provider does not name \
                         the domain {domain}, so it cannot bind the user by email address"
                    ),
                );
            }
            if let Err(refusal) = admit(&user) {
                return Ok(Err(refusal));
            }
            user.id
        }
        None => {
            if !provider.auto_create_users {
                return refuse(
                    "no_account",
                    "there is no user with this email address, and the provider does not create \
                     users"
                        .into(),
                );
            }
            if !provider.names(domain) {
                return refuse(
                    "domain_not_allowed",
                    format!("the domain {domain} may not sign in through this provider"),
                );
            }
            let name = identity
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| name.chars().take(200).collect::<String>())
                .unwrap_or_else(|| email.split('@').next().unwrap_or_default().to_owned());
            let user = authz::create_user(&mut tx, email, &name, false).await?;
            if let Some(role_id) = provider.default_role_id {
                sqlx::query(
                    "INSERT INTO role_assignment (id, user_id, role_id, company_id)
                     VALUES ($1, $2, $3, $4)",
                )
                .bind(new_id())
                .bind(user.id)
                .bind(role_id)
                .bind(provider.default_company_id)
                .execute(&mut *tx)
                .await?;
            }
            known.user_id = Some(user.id);
            user.id
        }
    };
    sqlx::query("INSERT INTO sso_identity (user_id, provider_id, subject) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(provider.id)
        .bind(&identity.subject)
        .execute(&mut *tx)
        .await?;
    events::record_on(
        &mut tx,
        "sso_identity",
        email,
        user_id,
        &provider.slug,
        &format!("bound at first sign-in to subject {}", identity.subject),
    )
    .await?;
    tx.commit().await?;
    Ok(Ok(user_id))
}

// ----------------------------------------------------------------- exchange

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange {
    code: String,
}

/// Hand over the token of a finished sign-in, once.
async fn exchange(
    State(sso): State<Sso>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let input: Exchange = erpcraft_kernel::input::parse(parse_body(&body)?, "input")?;
    let pool = sso.kernel.pool();
    // Taking the row out is what makes the code good once.
    let row: Option<(Uuid, String, String, Option<String>, bool)> = sqlx::query_as(
        "DELETE FROM sso_login_code c USING sso_provider p
         WHERE c.code_hash = $1 AND p.id = c.provider_id
         RETURNING c.user_id, p.slug, c.browser_hash, c.cookie_id, c.expires_at > now()",
    )
    .bind(hash_token(&input.code))
    .fetch_optional(pool)
    .await
    .map_err(Error::from)?;
    let Some((user_id, slug, browser_hash, cookie_id, live)) = row else {
        return Err(Error::Unauthorized.into());
    };
    let cookie =
        cookie_id.filter(|id| has_browser_cookie(&headers, &sso.cookie_name(id), &browser_hash));
    let refused = if !live {
        Some("the login code has expired")
    } else if cookie.is_none() {
        Some("the login code was presented by another browser")
    } else {
        None
    };
    // The user as they are now, which may not be what they were when the
    // provider signed them in a minute ago.
    let (active, is_admin, trusted): (bool, bool, bool) = sqlx::query_as(
        "SELECT COALESCE(u.active, false), COALESCE(u.is_admin, false),
                COALESCE(p.trusted_for_admins, false)
         FROM (SELECT 1) AS one
         LEFT JOIN app_user u ON u.id = $1
         LEFT JOIN sso_provider p ON p.slug = $2",
    )
    .bind(user_id)
    .bind(&slug)
    .fetch_one(pool)
    .await
    .map_err(Error::from)?;
    let refused = refused
        .or((!active).then_some("the user is inactive"))
        .or((is_admin && !trusted).then_some(
            "the user is an administrator, and the provider is not trusted for administrators",
        ));
    if let Some(reason) = refused {
        events::record(
            pool,
            AuthEvent {
                kind: "sso",
                provider: Some(&slug),
                user_id: Some(user_id),
                refused: Some(reason),
                ..AuthEvent::default()
            },
        )
        .await;
        return Err(Error::Unauthorized.into());
    }

    let mut conn = pool.acquire().await.map_err(Error::from)?;
    let session = issue_token(
        &mut conn,
        user_id,
        "login",
        None,
        LOGIN_TOKEN_DAYS,
        Some(&format!("sso:{slug}")),
    )
    .await?;
    let mut response = erpcraft_api::ok(json!(session))?;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(id) = cookie {
        headers.insert(SET_COOKIE, sso.cookie(&id, "", 0));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_is_a_path_on_this_server() {
        for ok in ["/", "/c/acme", "/c/acme/e/account?x=1&y=2#top", "/a%20b"] {
            assert_eq!(local_path(ok).as_deref(), Some(ok), "{ok}");
        }
        for refused in [
            "",
            "https://evil.example/",
            "http://evil.example/",
            "//evil.example/",
            "/\\evil.example/",
            "\\\\evil.example",
            "/a\\b",
            "c/acme",
            "javascript:alert(1)",
            "/a\nb",
            " /a",
        ] {
            assert_eq!(local_path(refused), None, "{refused:?}");
        }
        assert_eq!(local_path(&format!("/{}", "a".repeat(MAX_RETURN_TO))), None);
    }

    #[test]
    fn only_the_cookie_of_exactly_the_attempts_name_matches() {
        let mut headers = HeaderMap::new();
        headers.append(
            COOKIE,
            HeaderValue::from_static("theme=dark; erpcraft_sso_ab12cd34=one; other=x"),
        );
        headers.append(
            COOKIE,
            HeaderValue::from_static("__Host-erpcraft_sso_ffff0000=two"),
        );
        assert!(has_browser_cookie(
            &headers,
            "__Host-erpcraft_sso_ffff0000",
            &hash_token("two")
        ));
        assert!(has_browser_cookie(
            &headers,
            "erpcraft_sso_ab12cd34",
            &hash_token("one")
        ));
        // The right value under another name, as a sibling subdomain can
        // plant it, does not count: neither without the prefix, nor with
        // another attempt's id.
        assert!(!has_browser_cookie(
            &headers,
            "__Host-erpcraft_sso_ab12cd34",
            &hash_token("one")
        ));
        assert!(!has_browser_cookie(
            &headers,
            "__Host-erpcraft_sso_00000000",
            &hash_token("two")
        ));
        // Nor does a name that only begins the same.
        assert!(!has_browser_cookie(
            &headers,
            "erpcraft_sso_ab12cd3",
            &hash_token("one")
        ));
    }

    #[test]
    fn over_https_the_cookie_is_a_host_cookie() {
        assert_eq!(
            set_cookie(true, "abc", "v", 600),
            "__Host-erpcraft_sso_abc=v; Path=/; Max-Age=600; HttpOnly; SameSite=Lax; Secure"
        );
        // Plain http, which is allowed only on this machine, cannot use
        // the prefix, which needs Secure.
        assert_eq!(
            set_cookie(false, "abc", "v", 600),
            "erpcraft_sso_abc=v; Path=/; Max-Age=600; HttpOnly; SameSite=Lax"
        );
        assert_eq!(cookie_id().len(), 16);
    }

    #[test]
    fn random_values_fit_a_pkce_verifier() {
        let value = random();
        assert_eq!(value.len(), 64);
        assert!(value.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(value, random());
    }
}
