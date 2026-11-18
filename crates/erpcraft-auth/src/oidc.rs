//! The conversation with an OpenID Connect provider: its discovery
//! document, the exchange of an authorization code, its signing keys, and
//! the checks on the ID token it returns.
//!
//! Every URL called here comes from the provider's configuration or from
//! its discovery document, never from a request. All of them must be
//! https; answers are limited in size and in time; redirects are not
//! followed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{AlgorithmFamily, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use url::Url;

use crate::sso::check_endpoint;

/// The whole of one request to a provider.
const TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest answer read from a provider. Discovery documents and key
/// sets are a few kilobytes.
const MAX_BODY: usize = 256 * 1024;
/// How long a discovery document and a key set are used before they are
/// read again.
const CACHE_FOR: Duration = Duration::from_secs(3600);
/// A token signed with a key that is not in the cached set makes the keys
/// be read again, since providers change them. Not more often than this,
/// so that forged tokens cannot make this server hammer the provider.
const REFETCH_KEYS_EVERY: Duration = Duration::from_secs(60);
/// Clocks differ. Seconds allowed either way.
const LEEWAY: i64 = 60;
/// An ID token comes straight from the token endpoint, so it is fresh.
const MAX_TOKEN_AGE: i64 = 600;

/// Why a sign-in did not go through.
#[derive(Debug)]
pub(crate) struct Refusal {
    /// For the sign-in page: one of a few fixed words.
    pub code: &'static str,
    /// For the administrator's log of sign-ins.
    pub reason: String,
}

impl Refusal {
    pub(crate) fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Refusal {
            code,
            reason: reason.into(),
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Refusal::new("provider_unavailable", reason)
    }

    fn token(reason: impl Into<String>) -> Self {
        Refusal::new("invalid_token", reason)
    }
}

/// What a provider's discovery document says, checked.
#[derive(Debug)]
pub(crate) struct Discovery {
    pub authorization_endpoint: Url,
    token_endpoint: Url,
    jwks_uri: Url,
    /// Whether the client secret goes in the form instead of the
    /// `Authorization` header.
    secret_in_form: bool,
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

struct Cached {
    discovery: Arc<Discovery>,
    discovery_at: Instant,
    keys: Option<(Arc<JwkSet>, Instant)>,
    /// When the keys were last read again because of an unknown key.
    refetched_at: Option<Instant>,
}

/// Who the provider says has signed in.
#[derive(Debug)]
pub(crate) struct Identity {
    /// Who the user is at the provider (`sub`): never reassigned, unlike
    /// an email address.
    pub subject: String,
    pub email: Option<String>,
    /// Whether the provider vouches for the address.
    pub email_verified: bool,
    pub name: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn as_slice(&self) -> &[String] {
        match self {
            Audience::One(one) => std::slice::from_ref(one),
            Audience::Many(many) => many,
        }
    }
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: f64,
    iat: f64,
    nonce: Option<String>,
    azp: Option<String>,
    email: Option<String>,
    email_verified: Option<Value>,
    /// Microsoft Entra ID's optional claim: the email's domain owner is
    /// verified.
    xms_edov: Option<Value>,
    name: Option<String>,
}

/// Providers send `true`; a few send `"true"`.
fn is_true(value: &Option<Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

pub(crate) struct Oidc {
    client: reqwest::Client,
    allow_insecure: bool,
    /// By issuer.
    cache: Mutex<HashMap<String, Cached>>,
}

impl Oidc {
    pub(crate) fn new(allow_insecure: bool) -> Result<Oidc, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // A redirect could lead anywhere, past the checks made here.
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!allow_insecure)
            .user_agent(concat!("erpcraft-auth/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Oidc {
            client,
            allow_insecure,
            cache: Mutex::default(),
        })
    }

    /// Send a request and read at most [`MAX_BODY`] bytes of the answer.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<(reqwest::StatusCode, Vec<u8>), Refusal> {
        let mut response = request.send().await.map_err(|error| {
            Refusal::unavailable(format!("{what} could not be reached: {}", describe(&error)))
        })?;
        let status = response.status();
        let mut body = Vec::new();
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                Refusal::unavailable(format!("{what} could not be read: {}", describe(&error)))
            })?;
            let Some(chunk) = chunk else { break };
            if body.len() + chunk.len() > MAX_BODY {
                return Err(Refusal::unavailable(format!(
                    "{what} answered with more than {MAX_BODY} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        what: &str,
    ) -> Result<T, Refusal> {
        let request = self
            .client
            .get(url.clone())
            .header("accept", "application/json");
        let (status, body) = self.send(request, what).await?;
        if !status.is_success() {
            return Err(Refusal::unavailable(format!("{what} answered {status}")));
        }
        serde_json::from_slice(&body)
            .map_err(|error| Refusal::unavailable(format!("{what} is not as expected: {error}")))
    }

    fn endpoint(&self, text: &str, what: &str) -> Result<Url, Refusal> {
        check_endpoint(text, self.allow_insecure).map_err(|problem| {
            Refusal::unavailable(format!("the provider's {what} {problem}: {text}"))
        })
    }

    /// The provider's discovery document, from the cache or from
    /// `{issuer}/.well-known/openid-configuration`.
    pub(crate) async fn discovery(&self, issuer: &str) -> Result<Arc<Discovery>, Refusal> {
        if let Some(cached) = self.cache.lock().await.get(issuer)
            && cached.discovery_at.elapsed() < CACHE_FOR
        {
            return Ok(cached.discovery.clone());
        }
        // Checked when the provider was saved, and again now: the setting
        // may have changed, and the row may have been written another way.
        self.endpoint(issuer, "issuer")?;
        let url = self.endpoint(
            &format!(
                "{}/.well-known/openid-configuration",
                issuer.trim_end_matches('/')
            ),
            "discovery document",
        )?;
        let document: DiscoveryDocument = self
            .get_json(&url, "the provider's discovery document")
            .await?;
        // OpenID Connect Discovery 4.3: the two must be identical.
        if document.issuer != issuer {
            return Err(Refusal::unavailable(format!(
                "the provider states its issuer as '{}', not '{issuer}'; set the provider's \
                 issuer to exactly what it states",
                document.issuer
            )));
        }
        let methods = document.token_endpoint_auth_methods_supported;
        let discovery = Arc::new(Discovery {
            authorization_endpoint: self
                .endpoint(&document.authorization_endpoint, "authorization endpoint")?,
            token_endpoint: self.endpoint(&document.token_endpoint, "token endpoint")?,
            jwks_uri: self.endpoint(&document.jwks_uri, "key set address")?,
            // The header is the default when the document does not say.
            secret_in_form: methods.is_some_and(|methods| {
                !methods.iter().any(|m| m == "client_secret_basic")
                    && methods.iter().any(|m| m == "client_secret_post")
            }),
        });
        self.cache.lock().await.insert(
            issuer.to_owned(),
            Cached {
                discovery: discovery.clone(),
                discovery_at: Instant::now(),
                keys: None,
                refetched_at: None,
            },
        );
        Ok(discovery)
    }

    /// Exchange an authorization code for the ID token.
    pub(crate) async fn exchange(
        &self,
        discovery: &Discovery,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
        pkce_verifier: &str,
    ) -> Result<String, Refusal> {
        // Built before anything is awaited: the form builder cannot be
        // held across an await.
        let (body, authorization) = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", "authorization_code")
                .append_pair("code", code)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("code_verifier", pkce_verifier);
            if discovery.secret_in_form {
                form.append_pair("client_id", client_id)
                    .append_pair("client_secret", client_secret);
                (form.finish(), None)
            } else {
                // RFC 6749 2.3.1: each part is form-encoded before the two
                // are joined and base64-encoded.
                let encode = |part: &str| {
                    url::form_urlencoded::byte_serialize(part.as_bytes()).collect::<String>()
                };
                let credentials =
                    STANDARD.encode(format!("{}:{}", encode(client_id), encode(client_secret)));
                (form.finish(), Some(format!("Basic {credentials}")))
            }
        };
        let mut request = self
            .client
            .post(discovery.token_endpoint.clone())
            .header("accept", "application/json")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body);
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        let (status, body) = self.send(request, "the provider's token endpoint").await?;

        #[derive(Deserialize)]
        struct Answer {
            id_token: Option<String>,
            error: Option<String>,
            error_description: Option<String>,
        }
        let answer: Option<Answer> = serde_json::from_slice(&body).ok();
        if !status.is_success() {
            let detail = answer
                .and_then(|a| {
                    a.error.map(|error| match a.error_description {
                        Some(description) => format!(": {error}: {description}"),
                        None => format!(": {error}"),
                    })
                })
                .unwrap_or_default();
            return Err(Refusal::new(
                "provider_refused",
                format!("the provider's token endpoint answered {status}{detail}"),
            ));
        }
        answer.and_then(|a| a.id_token).ok_or_else(|| {
            Refusal::unavailable("the provider's token endpoint returned no ID token")
        })
    }

    async fn fetch_keys(
        &self,
        issuer: &str,
        discovery: &Discovery,
    ) -> Result<Arc<JwkSet>, Refusal> {
        let keys: Arc<JwkSet> = Arc::new(
            self.get_json(&discovery.jwks_uri, "the provider's key set")
                .await?,
        );
        if let Some(cached) = self.cache.lock().await.get_mut(issuer) {
            cached.keys = Some((keys.clone(), Instant::now()));
        }
        Ok(keys)
    }

    /// The key an ID token says it was signed with. A key that is not in
    /// the cached set makes the set be read again, once in a while.
    async fn key(
        &self,
        issuer: &str,
        discovery: &Discovery,
        kid: Option<&str>,
        family: AlgorithmFamily,
    ) -> Result<Jwk, Refusal> {
        let cached = self
            .cache
            .lock()
            .await
            .get(issuer)
            .and_then(|c| c.keys.clone())
            .filter(|(_, at)| at.elapsed() < CACHE_FOR);
        if let Some((keys, _)) = cached {
            if let Some(key) = find_key(&keys, kid, family) {
                return Ok(key);
            }
            // Not known: the provider may have changed its keys.
            let mut cache = self.cache.lock().await;
            let recently = cache
                .get(issuer)
                .and_then(|c| c.refetched_at)
                .is_some_and(|at| at.elapsed() < REFETCH_KEYS_EVERY);
            if recently {
                return Err(unknown_key(kid));
            }
            if let Some(entry) = cache.get_mut(issuer) {
                entry.refetched_at = Some(Instant::now());
            }
        }
        let keys = self.fetch_keys(issuer, discovery).await?;
        find_key(&keys, kid, family).ok_or_else(|| unknown_key(kid))
    }

    /// Check an ID token and return who it is about.
    ///
    /// OpenID Connect Core 3.1.3.7: the signature against the provider's
    /// keys, with an algorithm that has a public key; the issuer; the
    /// audience, and the authorized party when there is one; that it has
    /// not expired and was issued just now; and the nonce of this sign-in.
    pub(crate) async fn identity(
        &self,
        issuer: &str,
        client_id: &str,
        discovery: &Discovery,
        id_token: &str,
        nonce: &str,
    ) -> Result<Identity, Refusal> {
        let header = jsonwebtoken::decode_header(id_token)
            .map_err(|error| Refusal::token(format!("the ID token cannot be read: {error}")))?;
        // A shared-secret algorithm would let anyone who knows a public
        // value sign tokens; "none" is not an algorithm at all here.
        let family = header.alg.family();
        if family == AlgorithmFamily::Hmac {
            return Err(Refusal::token(format!(
                "the ID token is signed with {:?}, which has no public key",
                header.alg
            )));
        }
        let jwk = self
            .key(issuer, discovery, header.kid.as_deref(), family)
            .await?;
        let key = DecodingKey::from_jwk(&jwk).map_err(|error| {
            Refusal::token(format!("the provider's key cannot be used: {error}"))
        })?;

        let mut validation = Validation::new(header.alg);
        validation.leeway = LEEWAY as u64;
        validation.validate_nbf = true;
        validation.set_audience(&[client_id]);
        validation.set_issuer(&[issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims: Claims = jsonwebtoken::decode(id_token, &key, &validation)
            .map_err(|error| {
                Refusal::token(match error.kind() {
                    ErrorKind::InvalidSignature => {
                        "the ID token's signature is not valid".to_owned()
                    }
                    ErrorKind::ExpiredSignature => "the ID token has expired".to_owned(),
                    ErrorKind::InvalidIssuer => "the ID token is from another issuer".to_owned(),
                    ErrorKind::InvalidAudience => "the ID token is for another client".to_owned(),
                    _ => format!("the ID token is not valid: {error}"),
                })
            })?
            .claims;
        check_claims(&claims, issuer, client_id, nonce, now()).map_err(Refusal::token)?;

        let email_verified = is_true(&claims.email_verified) || is_true(&claims.xms_edov);
        Ok(Identity {
            subject: claims.sub,
            email: claims.email,
            email_verified,
            name: claims.name,
        })
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The checks on the claims, made here as well as by the JWT library, on
/// values of known types.
fn check_claims(
    claims: &Claims,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> Result<(), String> {
    if claims.iss != issuer {
        return Err("the ID token is from another issuer".into());
    }
    let audience = claims.aud.as_slice();
    if !audience.iter().any(|aud| aud == client_id) {
        return Err("the ID token is for another client".into());
    }
    // With several audiences the token must say which client asked.
    let party_ok = match &claims.azp {
        Some(azp) => azp == client_id,
        None => audience.len() == 1,
    };
    if !party_ok {
        return Err("the ID token was issued to another client".into());
    }
    if !claims.exp.is_finite() || !claims.iat.is_finite() {
        return Err("the ID token's times cannot be read".into());
    }
    if (claims.exp as i64) + LEEWAY <= now {
        return Err("the ID token has expired".into());
    }
    let issued = claims.iat as i64;
    if issued > now + LEEWAY {
        return Err("the ID token is dated in the future".into());
    }
    if issued < now - MAX_TOKEN_AGE - LEEWAY {
        return Err("the ID token is too old".into());
    }
    let same_nonce = claims
        .nonce
        .as_ref()
        .is_some_and(|given| bool::from(given.as_bytes().ct_eq(nonce.as_bytes())));
    if !same_nonce {
        return Err("the ID token does not belong to this sign-in (nonce)".into());
    }
    Ok(())
}

fn unknown_key(kid: Option<&str>) -> Refusal {
    Refusal::token(match kid {
        Some(kid) => {
            format!("the ID token is signed with a key the provider does not publish: {kid}")
        }
        None => {
            "the ID token does not say which key signed it, and the provider has several".to_owned()
        }
    })
}

fn family_of(jwk: &Jwk) -> Option<AlgorithmFamily> {
    use jsonwebtoken::jwk::AlgorithmParameters;
    match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Some(AlgorithmFamily::Rsa),
        AlgorithmParameters::EllipticCurve(_) => Some(AlgorithmFamily::Ec),
        AlgorithmParameters::OctetKeyPair(_) => Some(AlgorithmFamily::Ed),
        _ => None,
    }
}

/// The signing key with this id; or, for a token that names no key, the
/// provider's only signing key of the right kind.
fn find_key(keys: &JwkSet, kid: Option<&str>, family: AlgorithmFamily) -> Option<Jwk> {
    let mut usable = keys.keys.iter().filter(|jwk| {
        family_of(jwk) == Some(family)
            && !matches!(jwk.common.public_key_use, Some(PublicKeyUse::Encryption))
    });
    match kid {
        Some(kid) => usable
            .find(|jwk| jwk.common.key_id.as_deref() == Some(kid))
            .cloned(),
        None => {
            let first = usable.next()?;
            usable.next().is_none().then(|| first.clone())
        }
    }
}

/// Whether an algorithm name is one whose tokens are accepted.
#[cfg(test)]
fn accepted(alg: jsonwebtoken::Algorithm) -> bool {
    alg.family() != AlgorithmFamily::Hmac
}

/// A request error with its causes, which is where the reason is, and
/// without the URL.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "no answer in time".to_owned();
    }
    let mut text = if error.is_connect() {
        "could not connect".to_owned()
    } else {
        "the request failed".to_owned()
    };
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::Algorithm;

    use super::*;

    fn claims(json: Value) -> Claims {
        serde_json::from_value(json).unwrap()
    }

    fn good() -> Value {
        serde_json::json!({
            "iss": "https://issuer.example", "sub": "u1", "aud": "client",
            "exp": 2000, "iat": 1000, "nonce": "n1", "email": "a@example.com",
        })
    }

    fn check(json: Value) -> Result<(), String> {
        check_claims(
            &claims(json),
            "https://issuer.example",
            "client",
            "n1",
            1010,
        )
    }

    fn with(key: &str, value: Value) -> Value {
        let mut json = good();
        json[key] = value;
        json
    }

    #[test]
    fn claims_are_checked() {
        assert_eq!(check(good()), Ok(()));
        assert!(check(with("iss", "https://issuer.example/".into())).is_err());
        assert!(check(with("aud", "other".into())).is_err());
        assert!(check(with("nonce", "n2".into())).is_err());
        assert!(check(with("nonce", Value::Null)).is_err());
        // Expired, allowing for the clocks.
        assert!(check(with("exp", 949.into())).is_err());
        assert_eq!(check(with("exp", 951.into())), Ok(()));
        // Issued in the future, or long ago.
        assert!(check(with("iat", 1071.into())).is_err());
        assert_eq!(check(with("iat", 1069.into())), Ok(()));
        assert!(check(with("iat", 300.into())).is_err());
        // Times as fractions are read.
        assert_eq!(check(with("iat", serde_json::json!(1000.5))), Ok(()));
    }

    #[test]
    fn several_audiences_need_the_authorized_party() {
        let both = serde_json::json!(["client", "other"]);
        assert!(check(with("aud", both.clone())).is_err());
        let mut json = with("aud", both);
        json["azp"] = "client".into();
        assert_eq!(check(json.clone()), Ok(()));
        json["azp"] = "other".into();
        assert!(check(json).is_err());
        assert!(check(with("azp", "other".into())).is_err());
        assert!(check(with("aud", serde_json::json!(["other"]))).is_err());
    }

    #[test]
    fn only_algorithms_with_a_public_key_are_accepted() {
        for alg in [
            Algorithm::RS256,
            Algorithm::PS256,
            Algorithm::ES256,
            Algorithm::EdDSA,
        ] {
            assert!(accepted(alg), "{alg:?}");
        }
        for alg in [Algorithm::HS256, Algorithm::HS384, Algorithm::HS512] {
            assert!(!accepted(alg), "{alg:?}");
        }
    }

    #[test]
    fn verified_is_true_or_the_text_true() {
        assert!(is_true(&Some(Value::Bool(true))));
        assert!(is_true(&Some("true".into())));
        assert!(!is_true(&Some(Value::Bool(false))));
        assert!(!is_true(&Some("yes".into())));
        assert!(!is_true(&Some(1.into())));
        assert!(!is_true(&None));
    }
}
