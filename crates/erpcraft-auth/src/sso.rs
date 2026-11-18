//! Single sign-on providers: what an administrator configures.
//!
//! A provider is any OpenID Connect issuer (Microsoft Entra ID, Google,
//! Okta, Keycloak, Authentik). Providers are records of the read-only
//! entity `sso_provider` and are written through the commands here, which
//! are the only way in for the client secret. The sign-in itself is in
//! [`crate::flow`], the conversation with the provider in [`crate::oidc`].

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, EntityMeta, Error, FieldMeta, FieldType, JsonSchema, PgConnection,
    Result, async_trait, new_id, schemars,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use url::{Host, Url};
use uuid::Uuid;

use crate::events;

/// The address users reach this server at, such as `https://erp.example.com`.
/// The redirect URI registered with a provider is built from it, never
/// from the headers of a request.
pub const PUBLIC_URL: &str = "ERPCRAFT_PUBLIC_URL";
/// `1` lets a provider's issuer be plain http on this machine, for
/// development and tests.
pub const ALLOW_INSECURE: &str = "ERPCRAFT_SSO_ALLOW_INSECURE";

pub(crate) const READ: &str = "sso_provider:read";
/// Every change to a provider or to the identity a user is bound to. A
/// provider decides who its users are, so whoever holds this can sign in
/// as any user who is not an administrator. One permission, so that there
/// is no lesser one to grant by mistake.
pub(crate) const MANAGE: &str = "sso_provider:manage";
/// Needed besides, for a provider that grants a role to new users.
const ASSIGN_ROLE: &str = "user:assign_role";
/// The entry of `allowed_domains` that says any domain.
pub(crate) const ANY_DOMAIN: &str = "*";

const DEFAULT_SCOPES: &str = "openid email profile";
const MAX_SECRET: usize = 2000;

/// The settings of single sign-on that belong to the installation.
#[derive(Debug, Clone, Default)]
pub struct SsoConfig {
    public_url: Option<Url>,
    allow_insecure: bool,
}

impl SsoConfig {
    /// Read [`PUBLIC_URL`] and [`ALLOW_INSECURE`]. A public URL that cannot
    /// be used is logged, and single sign-on is then off.
    pub fn from_env() -> Self {
        let allow_insecure = std::env::var(ALLOW_INSECURE).is_ok_and(|value| value == "1");
        let public_url = std::env::var(PUBLIC_URL)
            .ok()
            .filter(|value| !value.trim().is_empty());
        match SsoConfig::new(public_url.as_deref(), allow_insecure) {
            Ok(config) => config,
            Err(error) => {
                tracing::error!(%error, "single sign-on is off: {PUBLIC_URL} cannot be used");
                SsoConfig {
                    public_url: None,
                    allow_insecure,
                }
            }
        }
    }

    /// `public_url` is https, or http on this machine. Without one, nobody
    /// can sign in through a provider.
    pub fn new(public_url: Option<&str>, allow_insecure: bool) -> Result<Self> {
        let public_url = match public_url {
            None => None,
            Some(text) => {
                let mut url = Url::parse(text.trim())
                    .map_err(|e| Error::validation(format!("the public URL is not a URL: {e}")))?;
                let plain_http_here = url.scheme() == "http" && is_loopback(&url);
                if url.scheme() != "https" && !plain_http_here {
                    return Err(Error::validation(
                        "the public URL must be https, or http on this machine",
                    ));
                }
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(Error::validation(
                        "the public URL cannot hold a user name or password",
                    ));
                }
                url.set_query(None);
                url.set_fragment(None);
                let path = url.path().trim_end_matches('/').to_owned();
                url.set_path(&path);
                Some(url)
            }
        };
        Ok(SsoConfig {
            public_url,
            allow_insecure,
        })
    }

    pub(crate) fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }

    /// The public URL without a trailing slash.
    pub(crate) fn public_base(&self) -> Option<String> {
        self.public_url
            .as_ref()
            .map(|url| url.as_str().trim_end_matches('/').to_owned())
    }

    /// Whether the browser reaches this server over https.
    pub(crate) fn is_https(&self) -> bool {
        self.public_url
            .as_ref()
            .is_some_and(|url| url.scheme() == "https")
    }

    /// Where a provider sends the browser back to. This exact value is
    /// what is registered with the provider.
    pub(crate) fn redirect_uri(&self, slug: &str) -> Option<String> {
        self.public_base()
            .map(|base| format!("{base}/api/auth/sso/{slug}/callback"))
    }
}

/// Whether a URL names this machine.
pub(crate) fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name == "localhost",
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    }
}

/// A URL this server may call or send a browser to: https, or with
/// `allow_insecure` also http on this machine. Never one that carries a
/// user name, a password or a fragment.
pub(crate) fn check_endpoint(text: &str, allow_insecure: bool) -> Result<Url, String> {
    let url = Url::parse(text).map_err(|e| format!("is not a URL: {e}"))?;
    let secure =
        url.scheme() == "https" || (allow_insecure && url.scheme() == "http" && is_loopback(&url));
    if !secure {
        return Err("must be an https URL".into());
    }
    if url.host().is_none() {
        return Err("has no host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("cannot hold a user name or password".into());
    }
    if url.fragment().is_some() {
        return Err("cannot have a fragment".into());
    }
    Ok(url)
}

// ---------------------------------------------------------------- the record

pub(crate) const COLUMNS: &str = "id, name, slug, issuer, client_id, scopes, allowed_domains, \
                                  auto_create_users, default_role_id, default_company_id, \
                                  require_verified_email, trusted_for_admins, active, created_at";

/// A provider as reads and commands return it: everything but the secret.
#[derive(Debug, Clone, Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct Provider {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
    pub allowed_domains: Vec<String>,
    pub auto_create_users: bool,
    pub default_role_id: Option<Uuid>,
    pub default_company_id: Option<Uuid>,
    pub require_verified_email: bool,
    pub trusted_for_admins: bool,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl Provider {
    /// Whether an address of this domain may sign in through the provider.
    pub(crate) fn allows(&self, domain: &str) -> bool {
        self.allowed_domains
            .iter()
            .any(|allowed| allowed == ANY_DOMAIN || allowed == domain)
    }

    /// Whether the provider names this domain. Only then can it bind a
    /// user of the domain by email address, or create one.
    pub(crate) fn names(&self, domain: &str) -> bool {
        self.allowed_domains.iter().any(|allowed| allowed == domain)
    }
}

/// A provider with what the administrator has to enter at the provider.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ProviderView {
    #[serde(flatten)]
    provider: Provider,
    /// The redirect URI to register with the provider, exactly as written.
    /// Null while the server has no public URL (`ERPCRAFT_PUBLIC_URL`).
    redirect_uri: Option<String>,
    /// Whether the provider has a client secret. It has none after its
    /// issuer or client id was changed, and cannot be active without one.
    has_secret: bool,
}

/// Read-only: providers are written through the commands of this module.
/// The client secret is not a field, and not a column of this table.
pub(crate) fn entity() -> EntityMeta {
    EntityMeta::new("sso_provider", "Single sign-on provider")
        .field(FieldMeta::new("name", "Name", FieldType::Text))
        .field(FieldMeta::new("slug", "Slug", FieldType::Text))
        .field(FieldMeta::new("issuer", "Issuer", FieldType::Text))
        .field(FieldMeta::new("client_id", "Client ID", FieldType::Text))
        .field(FieldMeta::new("scopes", "Scopes", FieldType::Text))
        .field(FieldMeta::new(
            "allowed_domains",
            "Allowed email domains",
            FieldType::TextArray,
        ))
        .field(FieldMeta::new(
            "auto_create_users",
            "Create users at first sign-in",
            FieldType::Bool,
        ))
        .field(FieldMeta::new(
            "default_role_id",
            "Role for new users",
            FieldType::link("role"),
        ))
        .field(FieldMeta::new(
            "default_company_id",
            "Company of that role",
            FieldType::link("company"),
        ))
        .field(FieldMeta::new(
            "require_verified_email",
            "Require a verified email",
            FieldType::Bool,
        ))
        .field(FieldMeta::new(
            "trusted_for_admins",
            "Administrators may sign in",
            FieldType::Bool,
        ))
        .field(FieldMeta::new("active", "Active", FieldType::Bool))
        .field(FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly())
        .order_by(&["name"])
        .search(&["name", "slug", "issuer"])
        // Like sso_provider.list: who signs in how is the installation's.
        .read_needs_global()
        .read_only()
}

// -------------------------------------------------------------------- checks

fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Error::field("name", "must have 1 to 100 characters"));
    }
    Ok(name.to_owned())
}

fn check_slug(slug: &str) -> Result<String> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-');
    // `exchange` is the route that hands out the token.
    if !ok || slug == "exchange" {
        return Err(Error::field(
            "slug",
            "must be 1 to 64 lowercase letters, digits and hyphens, such as 'entra', and not 'exchange'",
        ));
    }
    Ok(slug.to_owned())
}

/// The issuer is kept exactly as given: the provider's discovery document
/// and its tokens must state the same text.
fn check_issuer(issuer: &str, config: &SsoConfig) -> Result<String> {
    let issuer = issuer.trim();
    let url = check_endpoint(issuer, config.allow_insecure())
        .map_err(|problem| Error::field("issuer", problem))?;
    if url.query().is_some() {
        return Err(Error::field("issuer", "cannot have a query"));
    }
    Ok(issuer.to_owned())
}

fn check_client_id(client_id: &str) -> Result<String> {
    let client_id = client_id.trim();
    if client_id.is_empty() || client_id.len() > 500 {
        return Err(Error::field("client_id", "must have 1 to 500 characters"));
    }
    Ok(client_id.to_owned())
}

fn check_secret(secret: &str) -> Result<()> {
    if secret.is_empty() || secret.len() > MAX_SECRET || secret.chars().any(char::is_control) {
        return Err(Error::field(
            "client_secret",
            format!("must have 1 to {MAX_SECRET} characters"),
        ));
    }
    Ok(())
}

/// Scopes are separated by spaces. `openid` makes it OpenID Connect, and
/// `email` is how the user is found.
fn check_scopes(scopes: &str) -> Result<String> {
    let mut checked: Vec<&str> = Vec::new();
    for scope in scopes.split_whitespace() {
        // The characters RFC 6749 allows in a scope.
        let well_formed = scope
            .bytes()
            .all(|b| matches!(b, 0x21 | 0x23..=0x5b | 0x5d..=0x7e));
        if !well_formed {
            return Err(Error::field("scopes", format!("'{scope}' is not a scope")));
        }
        if !checked.contains(&scope) {
            checked.push(scope);
        }
    }
    for needed in ["openid", "email"] {
        if !checked.contains(&needed) {
            return Err(Error::field(
                "scopes",
                format!("must include '{needed}'; the usual value is '{DEFAULT_SCOPES}'"),
            ));
        }
    }
    Ok(checked.join(" "))
}

/// The email domains that may sign in. There must be at least one, so
/// that "anybody" is never what a provider means because nothing was
/// said: any domain is the single entry `*`.
fn check_domains(domains: &[String]) -> Result<Vec<String>> {
    let mut checked: Vec<String> = Vec::with_capacity(domains.len());
    for domain in domains {
        let domain = domain.trim().trim_start_matches('@').to_lowercase();
        let well_formed = domain == ANY_DOMAIN
            || (domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && domain
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.'));
        if !well_formed {
            return Err(Error::field(
                "allowed_domains",
                format!("'{domain}' is not an email domain such as 'example.com'"),
            ));
        }
        if !checked.contains(&domain) {
            checked.push(domain);
        }
    }
    if checked.is_empty() {
        return Err(Error::field(
            "allowed_domains",
            "must name the email domains that may sign in, such as [\"example.com\"]; \
             [\"*\"] says any domain",
        ));
    }
    if checked.len() > 1 && checked.iter().any(|d| d == ANY_DOMAIN) {
        return Err(Error::field(
            "allowed_domains",
            "is either a list of domains or [\"*\"] for any domain, not both",
        ));
    }
    Ok(checked)
}

/// What a provider says about who it may sign in.
struct Policy<'a> {
    allowed_domains: &'a [String],
    auto_create_users: bool,
    default_role_id: Option<Uuid>,
    default_company_id: Option<Uuid>,
    trusted_for_admins: bool,
}

/// Check a provider's policy as a whole, and that the caller may set it.
/// `had_role` and `was_trusted` say what the provider was before, for a
/// change; both false for a new one. `admits_anew` says whether the change
/// alters who the provider lets in: always for a new provider; for a
/// change, one to the issuer, the client id, the domains, creating users,
/// the role or its company, requiring a verified email, trust for
/// administrators, or activating it.
async fn check_policy(
    ctx: &mut Ctx<'_>,
    policy: &Policy<'_>,
    had_role: bool,
    was_trusted: bool,
    admits_anew: bool,
) -> Result<()> {
    // Otherwise anybody with an account at the provider, which for a
    // public provider is anybody, would become a user here.
    if policy.auto_create_users && policy.allowed_domains.iter().any(|d| d == ANY_DOMAIN) {
        return Err(Error::field(
            "allowed_domains",
            "must name the email domains when users are created at first sign-in; \
             any domain is not accepted there",
        ));
    }
    if policy.default_company_id.is_some() && policy.default_role_id.is_none() {
        return Err(Error::field(
            "default_company_id",
            "has no meaning without default_role_id",
        ));
    }
    // A provider with a role for new users grants that role to whoever it
    // lets in. So every change to who such a provider lets in, not only
    // to the role, is granting roles.
    if (policy.default_role_id.is_some() || had_role) && admits_anew {
        ctx.actor.require_global(ASSIGN_ROLE)?;
    }
    // And a provider administrators sign in through can make an
    // administrator of whoever controls it.
    if (policy.trusted_for_admins || was_trusted) && !ctx.actor.is_admin {
        return Err(Error::forbidden(
            "only an administrator can set up or change a provider that administrators sign in through",
        ));
    }
    if let Some(role_id) = policy.default_role_id
        && !exists(&mut *ctx.conn, "role", role_id).await?
    {
        return Err(Error::field("default_role_id", "must be a role"));
    }
    if let Some(company_id) = policy.default_company_id
        && !exists(&mut *ctx.conn, "company", company_id).await?
    {
        return Err(Error::field("default_company_id", "must be a company"));
    }
    Ok(())
}

async fn exists(conn: &mut PgConnection, table: &'static str, id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await?)
}

async fn has_secret(conn: &mut PgConnection, provider_id: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sso_provider_secret WHERE provider_id = $1)",
    )
    .bind(provider_id)
    .fetch_one(&mut *conn)
    .await?)
}

async fn locked(conn: &mut PgConnection, id: Uuid) -> Result<Provider> {
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM sso_provider WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found("sso_provider"))
}

/// A slug can be used once. Said here, because the database's own message
/// would name a constraint.
fn slug_taken(error: sqlx::Error) -> Error {
    match &error {
        sqlx::Error::Database(db) if db.constraint() == Some("sso_provider_slug_key") => {
            Error::duplicate("a provider with this slug already exists").with_field("slug")
        }
        _ => error.into(),
    }
}

// -------------------------------------------------------------------- create

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateInput {
    /// Shown on the sign-in page, for example "Microsoft".
    name: String,
    /// Part of the sign-in URLs and of the redirect URI: lowercase letters,
    /// digits and hyphens, for example "entra".
    slug: String,
    /// The provider's issuer URL, exactly as the provider states it. Its
    /// discovery document is read from
    /// `{issuer}/.well-known/openid-configuration`.
    issuer: String,
    /// The client (application) id the provider gave this installation.
    client_id: String,
    /// The client secret the provider gave this installation. It cannot be
    /// read back, only replaced with sso_provider.set_secret.
    #[schemars(extend("writeOnly" = true))]
    client_secret: String,
    /// Scopes to ask for, separated by spaces. Must include `openid` and
    /// `email`. `openid email profile` when omitted.
    scopes: Option<String>,
    /// Required. The email domains that may sign in through this provider,
    /// such as `["example.com"]`. Only a provider that names a user's
    /// domain can sign that user in for the first time. `["*"]` says any
    /// domain: such a provider signs in only users an administrator has
    /// bound to it with sso_identity.set, and cannot create users.
    allowed_domains: Vec<String>,
    /// Whether somebody who signs in for the first time becomes a user.
    /// `false` when omitted: only people who are users already can sign in.
    #[serde(default)]
    auto_create_users: bool,
    /// The role granted to a user created at first sign-in. Without it
    /// such a user can sign in and do nothing. Setting it takes the
    /// permission `user:assign_role` as well.
    default_role_id: Option<Uuid>,
    /// The company that role is granted for. Omit for all companies.
    default_company_id: Option<Uuid>,
    /// Whether the provider must state that the email address is verified
    /// (`email_verified`). `true` when omitted. Set to false only for a
    /// provider that does not send the claim and whose addresses your
    /// organization controls, such as a single-tenant Microsoft Entra ID
    /// application.
    require_verified_email: Option<bool>,
    /// Whether administrators may sign in through this provider. `false`
    /// when omitted. Whoever controls such a provider can sign in as an
    /// administrator, so only an administrator can set this.
    #[serde(default)]
    trusted_for_admins: bool,
    /// `true` when omitted.
    active: Option<bool>,
}

pub(crate) struct Create {
    pub config: SsoConfig,
}

#[async_trait]
impl Command for Create {
    type Input = CreateInput;
    type Output = ProviderView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sso_provider.create",
            "Add a single sign-on provider: an OpenID Connect issuer such as Microsoft Entra ID, \
             Google, Okta or Keycloak that users can sign in with. Returns the provider and the \
             redirect URI to register with it. The client secret is never returned. A provider \
             decides who its users are, so whoever may run this can sign in as any user who is \
             not an administrator: it takes the permission sso_provider:manage for all companies.",
        )
        .permission(MANAGE)
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CreateInput) -> Result<ProviderView> {
        ctx.actor.require_global(MANAGE)?;
        let name = check_name(&input.name)?;
        let slug = check_slug(&input.slug)?;
        let issuer = check_issuer(&input.issuer, &self.config)?;
        let client_id = check_client_id(&input.client_id)?;
        check_secret(&input.client_secret)?;
        let scopes = check_scopes(input.scopes.as_deref().unwrap_or(DEFAULT_SCOPES))?;
        let allowed_domains = check_domains(&input.allowed_domains)?;
        check_policy(
            ctx,
            &Policy {
                allowed_domains: &allowed_domains,
                auto_create_users: input.auto_create_users,
                default_role_id: input.default_role_id,
                default_company_id: input.default_company_id,
                trusted_for_admins: input.trusted_for_admins,
            },
            false,
            false,
            true,
        )
        .await?;

        let id = new_id();
        ctx.touch("sso_provider", id);
        let provider: Provider = sqlx::query_as(&format!(
            "INSERT INTO sso_provider (id, name, slug, issuer, client_id, scopes, allowed_domains,
                auto_create_users, default_role_id, default_company_id, require_verified_email,
                trusted_for_admins, active)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING {COLUMNS}"
        ))
        .bind(id)
        .bind(&name)
        .bind(&slug)
        .bind(&issuer)
        .bind(&client_id)
        .bind(&scopes)
        .bind(&allowed_domains)
        .bind(input.auto_create_users)
        .bind(input.default_role_id)
        .bind(input.default_company_id)
        .bind(input.require_verified_email.unwrap_or(true))
        .bind(input.trusted_for_admins)
        .bind(input.active.unwrap_or(true))
        .fetch_one(&mut *ctx.conn)
        .await
        .map_err(slug_taken)?;
        sqlx::query("INSERT INTO sso_provider_secret (provider_id, client_secret) VALUES ($1, $2)")
            .bind(id)
            .bind(&input.client_secret)
            .execute(&mut *ctx.conn)
            .await?;
        events::record_change(
            ctx,
            "sso_provider",
            &provider.slug,
            &format!(
                "created, for issuer {} and domains {}{}{}",
                provider.issuer,
                provider.allowed_domains.join(" "),
                if provider.auto_create_users {
                    ", creating users"
                } else {
                    ""
                },
                if provider.trusted_for_admins {
                    ", trusted for administrators"
                } else {
                    ""
                },
            ),
        )
        .await?;
        Ok(ProviderView {
            redirect_uri: self.config.redirect_uri(&provider.slug),
            has_secret: true,
            provider,
        })
    }
}

// -------------------------------------------------------------------- update

/// Tells a field that was left out from one that was sent as null.
fn sent<'de, T, D>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateInput {
    /// Provider id.
    id: Uuid,
    /// Shown on the sign-in page.
    name: Option<String>,
    /// Changing the slug changes the redirect URI, which must then be
    /// changed at the provider too.
    slug: Option<String>,
    /// The provider's issuer URL, exactly as the provider states it.
    /// Changing it discards the client secret and the identities users
    /// are bound to, and leaves the provider inactive until a new secret
    /// is set with sso_provider.set_secret.
    issuer: Option<String>,
    /// The client (application) id the provider gave this installation.
    /// Changing it has the same effect as changing the issuer.
    client_id: Option<String>,
    /// Scopes to ask for, separated by spaces. Must include `openid` and
    /// `email`.
    scopes: Option<String>,
    /// Replaces the email domains that may sign in. At least one;
    /// `["*"]` says any domain.
    allowed_domains: Option<Vec<String>>,
    /// Whether somebody who signs in for the first time becomes a user.
    auto_create_users: Option<bool>,
    /// The role granted to a user created at first sign-in; null for none.
    #[serde(default, deserialize_with = "sent")]
    default_role_id: Option<Option<Uuid>>,
    /// The company that role is granted for; null for all companies.
    #[serde(default, deserialize_with = "sent")]
    default_company_id: Option<Option<Uuid>>,
    /// Whether the provider must state that the email address is verified.
    require_verified_email: Option<bool>,
    /// Whether administrators may sign in through this provider. Only an
    /// administrator can change a provider that has this set.
    trusted_for_admins: Option<bool>,
    /// Nobody can sign in through an inactive provider. A provider can be
    /// active only while it has a client secret.
    active: Option<bool>,
}

pub(crate) struct Update {
    pub config: SsoConfig,
}

#[async_trait]
impl Command for Update {
    type Input = UpdateInput;
    type Output = ProviderView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sso_provider.update",
            "Change a single sign-on provider. Fields left out stay as they are. Changing the \
             issuer or the client id discards the client secret and the identities users are \
             bound to at the provider, and leaves it inactive: set the new secret with \
             sso_provider.set_secret, then activate it. Nobody can sign in through an inactive \
             provider; tokens issued before stay valid. Takes the permission \
             sso_provider:manage for all companies.",
        )
        .permission(MANAGE)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: UpdateInput) -> Result<ProviderView> {
        ctx.actor.require_global(MANAGE)?;
        ctx.touch("sso_provider", input.id);
        let current = locked(&mut *ctx.conn, input.id).await?;

        let name = match &input.name {
            Some(name) => check_name(name)?,
            None => current.name.clone(),
        };
        let slug = match &input.slug {
            Some(slug) => check_slug(slug)?,
            None => current.slug.clone(),
        };
        let issuer = match &input.issuer {
            Some(issuer) => check_issuer(issuer, &self.config)?,
            None => current.issuer.clone(),
        };
        let client_id = match &input.client_id {
            Some(client_id) => check_client_id(client_id)?,
            None => current.client_id.clone(),
        };
        let scopes = match &input.scopes {
            Some(scopes) => check_scopes(scopes)?,
            None => current.scopes.clone(),
        };
        let allowed_domains = match &input.allowed_domains {
            Some(domains) => check_domains(domains)?,
            // A provider from before domains were required has none, and
            // must be given some with its first change.
            None => check_domains(&current.allowed_domains)?,
        };
        let auto_create_users = input.auto_create_users.unwrap_or(current.auto_create_users);
        let default_role_id = input.default_role_id.unwrap_or(current.default_role_id);
        let default_company_id = input
            .default_company_id
            .unwrap_or(current.default_company_id);
        let require_verified_email = input
            .require_verified_email
            .unwrap_or(current.require_verified_email);
        let trusted_for_admins = input
            .trusted_for_admins
            .unwrap_or(current.trusted_for_admins);
        let admits_anew = issuer != current.issuer
            || client_id != current.client_id
            || allowed_domains != current.allowed_domains
            || auto_create_users != current.auto_create_users
            || default_role_id != current.default_role_id
            || default_company_id != current.default_company_id
            || require_verified_email != current.require_verified_email
            || trusted_for_admins != current.trusted_for_admins
            || (input.active == Some(true) && !current.active);
        check_policy(
            ctx,
            &Policy {
                allowed_domains: &allowed_domains,
                auto_create_users,
                default_role_id,
                default_company_id,
                trusted_for_admins,
            },
            current.default_role_id.is_some(),
            current.trusted_for_admins,
            admits_anew,
        )
        .await?;

        // The secret belongs to one client at one issuer. Kept across a
        // change of either, it would be sent to whatever the new issuer
        // is; and what the old issuer said about who is who says nothing
        // about the new one.
        let repointed = issuer != current.issuer || client_id != current.client_id;
        let mut discarded = String::new();
        let (active, secret) = if repointed {
            if input.active == Some(true) {
                return Err(Error::state(
                    "changing the issuer or the client id discards the client secret: set the \
                     new one with sso_provider.set_secret, then activate the provider",
                ));
            }
            sqlx::query("DELETE FROM sso_provider_secret WHERE provider_id = $1")
                .bind(input.id)
                .execute(&mut *ctx.conn)
                .await?;
            let unbound = sqlx::query("DELETE FROM sso_identity WHERE provider_id = $1")
                .bind(input.id)
                .execute(&mut *ctx.conn)
                .await?
                .rows_affected();
            // Sign-ins on their way were sent to the old issuer.
            sqlx::query("DELETE FROM sso_login_attempt WHERE provider_id = $1")
                .bind(input.id)
                .execute(&mut *ctx.conn)
                .await?;
            sqlx::query("DELETE FROM sso_login_code WHERE provider_id = $1")
                .bind(input.id)
                .execute(&mut *ctx.conn)
                .await?;
            discarded = format!(
                "; the client secret and {unbound} bound identities were discarded, and the \
                 provider is inactive until a new secret is set"
            );
            (false, false)
        } else {
            let secret = has_secret(&mut *ctx.conn, input.id).await?;
            let active = input.active.unwrap_or(current.active);
            if active && !secret {
                return Err(Error::state(
                    "the provider has no client secret: set one with sso_provider.set_secret \
                     before activating it",
                ));
            }
            (active, secret)
        };

        let provider: Provider = sqlx::query_as(&format!(
            "UPDATE sso_provider SET name = $2, slug = $3, issuer = $4, client_id = $5,
                scopes = $6, allowed_domains = $7, auto_create_users = $8, default_role_id = $9,
                default_company_id = $10, require_verified_email = $11, trusted_for_admins = $12,
                active = $13
             WHERE id = $1 RETURNING {COLUMNS}"
        ))
        .bind(input.id)
        .bind(&name)
        .bind(&slug)
        .bind(&issuer)
        .bind(&client_id)
        .bind(&scopes)
        .bind(&allowed_domains)
        .bind(auto_create_users)
        .bind(default_role_id)
        .bind(default_company_id)
        .bind(require_verified_email)
        .bind(trusted_for_admins)
        .bind(active)
        .fetch_one(&mut *ctx.conn)
        .await
        .map_err(slug_taken)?;

        let changes = [
            ("name", provider.name != current.name),
            ("slug", provider.slug != current.slug),
            ("issuer", provider.issuer != current.issuer),
            ("client_id", provider.client_id != current.client_id),
            ("scopes", provider.scopes != current.scopes),
            (
                "allowed_domains",
                provider.allowed_domains != current.allowed_domains,
            ),
            (
                "auto_create_users",
                provider.auto_create_users != current.auto_create_users,
            ),
            (
                "default_role_id",
                provider.default_role_id != current.default_role_id,
            ),
            (
                "default_company_id",
                provider.default_company_id != current.default_company_id,
            ),
            (
                "require_verified_email",
                provider.require_verified_email != current.require_verified_email,
            ),
            (
                "trusted_for_admins",
                provider.trusted_for_admins != current.trusted_for_admins,
            ),
            ("active", provider.active != current.active),
        ];
        let changed: Vec<&str> = changes
            .iter()
            .filter(|(_, changed)| *changed)
            .map(|(field, _)| *field)
            .collect();
        let what = if changed.is_empty() {
            "updated, changing nothing".to_owned()
        } else {
            format!("changed {}{discarded}", changed.join(", "))
        };
        events::record_change(ctx, "sso_provider", &provider.slug, &what).await?;
        Ok(ProviderView {
            redirect_uri: self.config.redirect_uri(&provider.slug),
            has_secret: secret,
            provider,
        })
    }
}

// ---------------------------------------------------------------- set secret

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetSecretInput {
    /// Provider id.
    id: Uuid,
    /// The new client secret from the provider.
    #[schemars(extend("writeOnly" = true))]
    client_secret: String,
}

pub(crate) struct SetSecret;

#[async_trait]
impl Command for SetSecret {
    type Input = SetSecretInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sso_provider.set_secret",
            "Set or replace the client secret of a single sign-on provider, for example when \
             the provider's secret is about to expire, or after the issuer or client id was \
             changed. Sign-ins from now on use the new one. It does not activate an inactive \
             provider: that is sso_provider.update with active true. The secret is never \
             returned. Takes the permission sso_provider:manage for all companies.",
        )
        .permission(MANAGE)
        .label("Set client secret")
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetSecretInput) -> Result<Value> {
        ctx.actor.require_global(MANAGE)?;
        ctx.touch("sso_provider", input.id);
        check_secret(&input.client_secret)?;
        let provider = locked(&mut *ctx.conn, input.id).await?;
        if provider.trusted_for_admins && !ctx.actor.is_admin {
            return Err(Error::forbidden(
                "only an administrator can change a provider that administrators sign in through",
            ));
        }
        sqlx::query(
            "INSERT INTO sso_provider_secret (provider_id, client_secret) VALUES ($1, $2)
             ON CONFLICT (provider_id) DO UPDATE SET client_secret = EXCLUDED.client_secret",
        )
        .bind(input.id)
        .bind(&input.client_secret)
        .execute(&mut *ctx.conn)
        .await?;
        events::record_change(
            ctx,
            "sso_provider",
            &provider.slug,
            "the client secret was set",
        )
        .await?;
        Ok(json!({ "id": input.id, "secret_replaced": true, "active": provider.active }))
    }
}

// -------------------------------------------------------------------- delete

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteInput {
    /// Provider id.
    id: Uuid,
}

pub(crate) struct Delete;

#[async_trait]
impl Command for Delete {
    type Input = DeleteInput;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "sso_provider.delete",
            "Delete a single sign-on provider with its client secret and the identities users \
             are bound to at it. Nobody can sign in through it any more. Its users stay, and \
             tokens issued before stay valid until they expire; the record of sign-ins stays \
             too. Takes the permission sso_provider:manage for all companies.",
        )
        .permission(MANAGE)
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: DeleteInput) -> Result<Value> {
        ctx.actor.require_global(MANAGE)?;
        ctx.touch("sso_provider", input.id);
        let provider = locked(&mut *ctx.conn, input.id).await?;
        if provider.trusted_for_admins && !ctx.actor.is_admin {
            return Err(Error::forbidden(
                "only an administrator can delete a provider that administrators sign in through",
            ));
        }
        sqlx::query("DELETE FROM sso_provider WHERE id = $1")
            .bind(input.id)
            .execute(&mut *ctx.conn)
            .await?;
        events::record_change(ctx, "sso_provider", &provider.slug, "deleted").await?;
        Ok(json!({ "id": input.id, "deleted": true }))
    }
}

// ---------------------------------------------------------------------- list

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListInput {}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ListOutput {
    /// Whether the server has a public URL. Without one nobody can sign in
    /// through a provider.
    configured: bool,
    items: Vec<ProviderView>,
}

pub(crate) struct List {
    pub config: SsoConfig,
}

#[async_trait]
impl Command for List {
    type Input = ListInput;
    type Output = ListOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "sso_provider.list",
            "The single sign-on providers with their settings and, for each, the redirect URI \
             to register with the provider and whether it has a client secret. Client secrets \
             are never returned.",
        )
        .permission(READ)
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, _input: ListInput) -> Result<ListOutput> {
        ctx.actor.require_global(READ)?;
        let providers: Vec<Provider> = sqlx::query_as(&format!(
            "SELECT {COLUMNS} FROM sso_provider ORDER BY name, slug"
        ))
        .fetch_all(&mut *ctx.conn)
        .await?;
        let with_secret: Vec<Uuid> =
            sqlx::query_scalar("SELECT provider_id FROM sso_provider_secret")
                .fetch_all(&mut *ctx.conn)
                .await?;
        Ok(ListOutput {
            configured: self.config.public_base().is_some(),
            items: providers
                .into_iter()
                .map(|provider| ProviderView {
                    redirect_uri: self.config.redirect_uri(&provider.slug),
                    has_secret: with_secret.contains(&provider.id),
                    provider,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_url_is_https_or_this_machine() {
        let config = SsoConfig::new(Some("https://erp.example.com/"), false).unwrap();
        assert_eq!(
            config.redirect_uri("entra").unwrap(),
            "https://erp.example.com/api/auth/sso/entra/callback"
        );
        assert!(config.is_https());
        let config = SsoConfig::new(Some("https://example.com/erp/?x=1#y"), false).unwrap();
        assert_eq!(
            config.redirect_uri("g").unwrap(),
            "https://example.com/erp/api/auth/sso/g/callback"
        );
        let config = SsoConfig::new(Some("http://127.0.0.1:8600"), false).unwrap();
        assert_eq!(
            config.redirect_uri("g").unwrap(),
            "http://127.0.0.1:8600/api/auth/sso/g/callback"
        );
        assert!(!config.is_https());
        assert!(SsoConfig::new(Some("http://localhost:5173"), false).is_ok());
        for refused in [
            "http://erp.example.com",
            "ftp://erp.example.com",
            "https://user:pw@erp.example.com",
            "erp.example.com",
        ] {
            assert!(SsoConfig::new(Some(refused), true).is_err(), "{refused}");
        }
        assert!(
            SsoConfig::new(None, false)
                .unwrap()
                .redirect_uri("g")
                .is_none()
        );
    }

    #[test]
    fn endpoints_are_https_unless_insecure_is_allowed_on_this_machine() {
        assert!(check_endpoint("https://login.example.com/token", false).is_ok());
        for refused in [
            "http://login.example.com/token",
            "http://127.0.0.1:9000/token",
            "http://localhost/token",
            "https://user@login.example.com/token",
            "https://login.example.com/token#x",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(check_endpoint(refused, false).is_err(), "{refused}");
        }
        assert!(check_endpoint("http://127.0.0.1:9000/token", true).is_ok());
        assert!(check_endpoint("http://localhost:9000/token", true).is_ok());
        assert!(check_endpoint("http://[::1]:9000/token", true).is_ok());
        // Allowing insecure issuers never reaches beyond this machine.
        assert!(check_endpoint("http://login.example.com/token", true).is_err());
        assert!(check_endpoint("http://10.0.0.5/token", true).is_err());
    }

    #[test]
    fn scopes_domains_and_slugs_are_checked() {
        assert_eq!(
            check_scopes(" openid  email profile openid ").unwrap(),
            "openid email profile"
        );
        assert!(check_scopes("email profile").is_err());
        assert!(check_scopes("openid profile").is_err());
        assert!(check_scopes("openid email \"x\"").is_err());

        assert_eq!(
            check_domains(&[
                "@Example.COM".into(),
                "example.com".into(),
                " b.org ".into()
            ])
            .unwrap(),
            ["example.com", "b.org"]
        );
        for refused in [
            "example",
            "a b.com",
            ".com",
            "exa_mple.com",
            "*.example.com",
        ] {
            assert!(check_domains(&[refused.into()]).is_err(), "{refused}");
        }
        // Any domain is said, never implied.
        assert!(check_domains(&[]).is_err());
        assert_eq!(check_domains(&["*".into()]).unwrap(), ["*"]);
        assert!(check_domains(&["*".into(), "example.com".into()]).is_err());

        assert!(check_slug("entra-id").is_ok());
        for refused in ["", "Entra", "a b", "-a", "a-", "exchange", "a/b"] {
            assert!(check_slug(refused).is_err(), "{refused}");
        }
    }
}
