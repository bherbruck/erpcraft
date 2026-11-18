//! Identity: how a caller proves who they are.
//!
//! Users sign in with a password, or through an OpenID Connect provider
//! (single sign-on, see [`flow`]), and receive an opaque access token. Only
//! a hash of the token is stored. A token may carry scopes that cap what it
//! can do below its user's rights, which is how an agent is given less than
//! the person it works for.
//!
//! This is a module like any other. The kernel holds the user directory and
//! decides what an actor may do; this crate only answers "who is this?".
//! An installation can replace it with another module that supplies an
//! [`Authenticator`].

use std::sync::{Arc, OnceLock};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use chrono::{DateTime, Utc};
use erpcraft_api::{ApiResult, bearer, ok, parse_body};
use erpcraft_kernel::authz::{self, User};
use erpcraft_kernel::{
    Actor, Authenticator, Command, CommandSpec, Ctx, DynCommand, EntityMeta, Error, JsonSchema,
    Kernel, Migration, Module, PgConnection, Result, async_trait, command, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

mod events;
mod flow;
mod identity;
mod oidc;
mod sso;

pub use sso::{ALLOW_INSECURE, PUBLIC_URL, SsoConfig};

use events::AuthEvent;

pub(crate) const LOGIN_TOKEN_DAYS: i32 = 30;
/// The longest a token made with `auth.create_token` lasts: a year. It
/// was ten; a token that outlives its user's rights by years is a risk
/// nobody watches.
const MAX_DERIVED_TOKEN_DAYS: i32 = 365;
const MIN_PASSWORD_LEN: usize = 10;

pub struct AuthModule;

impl Module for AuthModule {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "auth",
                sql: include_str!("../migrations/0001_auth.sql"),
            },
            Migration {
                version: 2,
                name: "sso",
                sql: include_str!("../migrations/0002_sso.sql"),
            },
            Migration {
                version: 3,
                name: "sso_identity",
                sql: include_str!("../migrations/0003_sso_identity.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        vec![sso::entity()]
    }

    /// The settings of single sign-on are read from the environment here,
    /// once, when the server starts.
    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        let config = SsoConfig::from_env();
        vec![
            command(UserCreate),
            command(TokenCreate),
            command(sso::Create {
                config: config.clone(),
            }),
            command(sso::Update {
                config: config.clone(),
            }),
            command(sso::SetSecret),
            command(sso::Delete),
            command(sso::List { config }),
            command(identity::Set),
            command(identity::Delete),
            command(identity::List),
            command(events::Events),
        ]
    }

    fn authenticator(&self) -> Option<Arc<dyn Authenticator>> {
        Some(Arc::new(TokenAuthenticator))
    }
}

/// Sign-in routes. Merge next to the API router. The settings of single
/// sign-on are read from the environment.
pub fn router(kernel: Kernel) -> Router {
    router_with(kernel, SsoConfig::from_env())
}

/// Like [`router`], with the settings of single sign-on given.
pub fn router_with(kernel: Kernel, sso: SsoConfig) -> Router {
    Router::new()
        .route("/api/auth/login", post(login_route))
        .route("/api/auth/logout", post(logout_route))
        .with_state(kernel.clone())
        .merge(flow::routes(kernel, sso))
}

// ---------------------------------------------------------------- passwords

fn hash_password(password: &str) -> Result<String> {
    // A random v4 UUID supplies 122 bits of salt from the OS generator.
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(Error::internal)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(Error::internal)
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Verified when the email is unknown, so that a wrong email costs the same
/// time as a wrong password.
fn dummy_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| hash_password("erpcraft-dummy-password").unwrap_or_default())
}

/// Give a user a password, replacing any previous one.
pub async fn set_password(conn: &mut PgConnection, user_id: Uuid, password: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(Error::validation(format!(
            "the password must have at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    let password = password.to_owned();
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(Error::internal)??;
    sqlx::query(
        "INSERT INTO user_password (user_id, password_hash) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET password_hash = EXCLUDED.password_hash, updated_at = now()",
    )
    .bind(user_id)
    .bind(&hash)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Create the first administrator. Does nothing once any user exists.
/// Returns whether a user was created.
pub async fn bootstrap_admin(kernel: &Kernel, email: &str, password: &str) -> Result<bool> {
    let mut tx = kernel.pool().begin().await?;
    // Serializes concurrent bootstraps.
    sqlx::query("LOCK TABLE app_user IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM app_user)")
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Ok(false);
    }
    let user = authz::create_user(&mut tx, email, "Administrator", true).await?;
    set_password(&mut tx, user.id, password).await?;
    tx.commit().await?;
    Ok(true)
}

// ------------------------------------------------------------------- tokens

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Session {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

pub(crate) fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

/// Issue a token for a user. The plain token is returned once and never stored.
pub async fn create_token(
    conn: &mut PgConnection,
    user_id: Uuid,
    name: &str,
    scopes: Option<&[String]>,
    days: i32,
) -> Result<Session> {
    issue_token(conn, user_id, name, scopes, days, None).await
}

/// Issue a token, noting how its user signed in: `password`, or `sso:` and
/// the provider's slug. `None` for a token that is not from a sign-in.
pub(crate) async fn issue_token(
    conn: &mut PgConnection,
    user_id: Uuid,
    name: &str,
    scopes: Option<&[String]>,
    days: i32,
    signed_in_with: Option<&str>,
) -> Result<Session> {
    if !(1..=3650).contains(&days) {
        return Err(Error::validation("a token must expire in 1 to 3650 days"));
    }
    let token = format!("erp_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO api_token (id, token_hash, user_id, name, scopes, expires_at, signed_in_with)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(days => $6), $7)
         RETURNING expires_at",
    )
    .bind(new_id())
    .bind(hash_token(&token))
    .bind(user_id)
    .bind(name)
    .bind(scopes)
    .bind(days)
    .bind(signed_in_with)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Session { token, expires_at })
}

/// Exchange an email and password for a token.
pub async fn login(kernel: &Kernel, email: &str, password: &str) -> Result<Session> {
    let email = email.trim().to_lowercase();
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT u.id, p.password_hash FROM app_user u
         JOIN user_password p ON p.user_id = u.id
         WHERE u.email = $1 AND u.active",
    )
    .bind(&email)
    .fetch_optional(kernel.pool())
    .await?;
    let (user_id, hash) = match row {
        Some((id, hash)) => (Some(id), hash),
        None => (None, dummy_hash().to_owned()),
    };
    let password = password.to_owned();
    let valid = tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .map_err(Error::internal)?;
    let signed_in = user_id.filter(|_| valid);
    events::record(
        kernel.pool(),
        AuthEvent {
            kind: "password",
            email: Some(&email),
            user_id,
            // The same words whichever was wrong, as for the caller.
            refused: signed_in
                .is_none()
                .then_some("wrong email address or password"),
            ..AuthEvent::default()
        },
    )
    .await;
    match signed_in {
        Some(id) => {
            let mut conn = kernel.pool().acquire().await?;
            issue_token(
                &mut conn,
                id,
                "login",
                None,
                LOGIN_TOKEN_DAYS,
                Some("password"),
            )
            .await
        }
        None => Err(Error::Unauthorized),
    }
}

/// Revoke a token.
pub async fn logout(kernel: &Kernel, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM api_token WHERE token_hash = $1")
        .bind(hash_token(token))
        .execute(kernel.pool())
        .await?;
    Ok(())
}

struct TokenAuthenticator;

#[async_trait]
impl Authenticator for TokenAuthenticator {
    async fn authenticate(&self, kernel: &Kernel, token: &str) -> Result<Actor> {
        // With the token, whether the provider it came through, if any, is
        // trusted for administrators now. A provider renamed or deleted
        // since is not.
        let row: Option<(Uuid, Option<Vec<String>>, Option<String>, bool)> = sqlx::query_as(
            "SELECT t.user_id, t.scopes, t.signed_in_with,
                    COALESCE(p.trusted_for_admins, false)
             FROM api_token t
             LEFT JOIN sso_provider p ON t.signed_in_with = 'sso:' || p.slug
             WHERE t.token_hash = $1 AND t.expires_at > now()",
        )
        .bind(hash_token(token))
        .fetch_optional(kernel.pool())
        .await?;
        let Some((user_id, scopes, signed_in_with, trusted)) = row else {
            return Err(Error::Unauthorized);
        };
        let mut actor = authz::load_actor(kernel.pool(), user_id, scopes).await?;
        // An administrator acts as one only on a credential that could
        // have signed an administrator in: a password, or a provider
        // trusted for administrators. A token from an untrusted provider,
        // or made from one, of a user made an administrator since, is
        // refused rather than quietly acting with fewer rights, so that
        // the user knows to sign in again.
        if actor.is_admin && untrusted_for_admins(signed_in_with.as_deref(), trusted) {
            return Err(Error::Unauthorized);
        }
        actor.credential = signed_in_with;
        Ok(actor)
    }
}

/// Whether a credential from `signed_in_with` must not act as an
/// administrator: it came through a single sign-on provider that is not
/// trusted for administrators.
pub(crate) fn untrusted_for_admins(signed_in_with: Option<&str>, trusted: bool) -> bool {
    signed_in_with.is_some_and(|how| how.starts_with("sso:")) && !trusted
}

// ------------------------------------------------------------------- routes

#[derive(Deserialize)]
struct Login {
    email: String,
    password: String,
}

async fn login_route(State(kernel): State<Kernel>, body: Bytes) -> ApiResult {
    let input: Login = serde_json::from_value(parse_body(&body)?)
        .map_err(|_| Error::validation("email and password are required"))?;
    let session = login(&kernel, &input.email, &input.password).await?;
    ok(json!(session))
}

async fn logout_route(State(kernel): State<Kernel>, headers: HeaderMap) -> ApiResult {
    logout(&kernel, bearer(&headers)?).await?;
    ok(json!({ "logged_out": true }))
}

// ----------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UserCreateInput {
    /// The address the user signs in with.
    email: String,
    /// The user's name, as shown to others.
    name: String,
    /// The password the user signs in with. Leave out for a user who
    /// signs in only through a single sign-on provider.
    #[schemars(extend("writeOnly" = true))]
    password: Option<String>,
    /// An administrator may do everything and is not subject to permission rules.
    #[serde(default)]
    is_admin: bool,
}

struct UserCreate;

#[async_trait]
impl Command for UserCreate {
    type Input = UserCreateInput;
    type Output = User;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "user.create",
            "Create a user who signs in with a password or, when no password is given, only \
             through a single sign-on provider.",
        )
        .permission("user:create")
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UserCreateInput) -> Result<User> {
        ctx.actor.require_global("user:create")?;
        if input.is_admin && !ctx.actor.is_admin {
            return Err(Error::forbidden(
                "only an administrator can create an administrator",
            ));
        }
        let user =
            authz::create_user(&mut *ctx.conn, &input.email, &input.name, input.is_admin).await?;
        if let Some(password) = &input.password {
            set_password(&mut *ctx.conn, user.id, password).await?;
        }
        ctx.touch("user", user.id);
        Ok(user)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TokenCreateInput {
    /// What the token is for, for example the name of an agent.
    name: String,
    /// Permission patterns such as `journal_entry:read` or `*:read`. The
    /// token can do only what both these and your own rights allow. Omit
    /// for a token with all of your rights.
    scopes: Option<Vec<String>>,
    /// Days until the token stops working: 90 when omitted, at most 365.
    expires_in_days: Option<i32>,
}

struct TokenCreate;

#[async_trait]
impl Command for TokenCreate {
    type Input = TokenCreateInput;
    type Output = Session;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "auth.create_token",
            "Create an access token for yourself, optionally limited to some permissions. \
             The token is shown once.",
        )
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: TokenCreateInput) -> Result<Session> {
        if ctx.actor.scopes.is_some() {
            return Err(Error::forbidden("a limited token cannot create tokens"));
        }
        let days = input.expires_in_days.unwrap_or(90);
        if !(1..=MAX_DERIVED_TOKEN_DAYS).contains(&days) {
            return Err(Error::field(
                "expires_in_days",
                format!("must be 1 to {MAX_DERIVED_TOKEN_DAYS}"),
            ));
        }
        // The new token carries on how the one in use was obtained, so it
        // is no more than the sign-in it came from: one from a provider
        // not trusted for administrators stays one.
        issue_token(
            &mut *ctx.conn,
            ctx.actor.user_id,
            input.name.trim(),
            input.scopes.as_deref(),
            days,
            ctx.actor.credential.as_deref(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_verify() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("anything", "not a hash"));
    }

    #[test]
    fn tokens_are_stored_hashed() {
        assert_ne!(hash_token("erp_abc"), "erp_abc");
        assert_eq!(hash_token("erp_abc"), hash_token("erp_abc"));
    }
}
