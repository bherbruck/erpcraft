//! The users the kernel knows, and how a credential becomes an actor.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use erpcraft_core::{Error, Result, new_id};
use erpcraft_db::{PgConnection, PgPool};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::actor::{Actor, Effect, Grant, PermissionRule};
use crate::Kernel;

/// Turns a credential into an actor. Exactly one module supplies it; see
/// [`Module::authenticator`](crate::Module::authenticator).
#[async_trait]
pub trait Authenticator: Send + Sync + 'static {
    /// Returns [`Error::Unauthorized`] for a credential that is unknown,
    /// expired or revoked.
    async fn authenticate(&self, kernel: &Kernel, token: &str) -> Result<Actor>;
}

#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// Add a user to the directory. Performs no permission check; callers
/// decide who may. The user cannot sign in until an identity module gives
/// them a credential.
pub async fn create_user(
    conn: &mut PgConnection,
    email: &str,
    name: &str,
    is_admin: bool,
) -> Result<User> {
    let email = email.trim().to_lowercase();
    if email.len() < 3 || !email.contains('@') {
        return Err(Error::validation("a valid email address is required"));
    }
    let user = sqlx::query_as(
        "INSERT INTO app_user (id, email, name, is_admin) VALUES ($1, $2, $3, $4)
         RETURNING id, email, name, is_admin, active, created_at",
    )
    .bind(new_id())
    .bind(&email)
    .bind(name.trim())
    .bind(is_admin)
    .fetch_one(&mut *conn)
    .await?;
    Ok(user)
}

/// Build the actor for an active user from their current roles. `scopes`
/// is the cap of the credential they presented, if it has one.
pub async fn load_actor(
    pool: &PgPool,
    user_id: Uuid,
    scopes: Option<Vec<String>>,
) -> Result<Actor> {
    // The user and their roles in one round trip: this runs for every
    // request.
    let user: Option<(String, String, bool, Value, Value)> = sqlx::query_as(
        "SELECT u.email, u.name, u.is_admin, u.attributes,
                COALESCE(jsonb_agg(jsonb_build_array(ra.company_id, r.permissions))
                             FILTER (WHERE r.id IS NOT NULL), '[]')
         FROM app_user u
         LEFT JOIN role_assignment ra ON ra.user_id = u.id
         LEFT JOIN role r ON r.id = ra.role_id
         WHERE u.id = $1 AND u.active
         GROUP BY u.id",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    let Some((email, name, is_admin, attributes, assignments)) = user else {
        return Err(Error::Unauthorized);
    };
    let assignments: Vec<(Option<Uuid>, Value)> = assignments
        .as_array()
        .into_iter()
        .flatten()
        .map(|pair| {
            let company_id = pair[0].as_str().and_then(|id| Uuid::parse_str(id).ok());
            (company_id, pair[1].clone())
        })
        .collect();
    let grants = assignments
        .into_iter()
        .map(|(company_id, permissions)| Grant {
            company_id,
            rules: permissions
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(stored_rule)
                .collect(),
        })
        .collect();
    Ok(Actor {
        user_id,
        email,
        name,
        is_admin,
        attributes,
        grants,
        scopes,
        credential: None,
    })
}

/// Read a rule that was validated when it was saved. If it can no longer
/// be read, for example after an upgrade changed what rules may contain,
/// it must not start allowing: an allow is dropped, and a deny is kept as a
/// rule that denies everything it names.
fn stored_rule(value: &Value) -> Option<PermissionRule> {
    match PermissionRule::from_json(value) {
        Ok(rule) => Some(rule),
        Err(error) => {
            tracing::warn!(%error, "a stored permission rule cannot be read");
            let permission = value.get("deny")?.as_str()?.to_owned();
            Some(PermissionRule {
                effect: Effect::Deny,
                permission,
                condition: None,
                fields: None,
                broken: true,
            })
        }
    }
}
