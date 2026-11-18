-- Single sign-on with OpenID Connect.

-- An identity provider users can sign in with. Written only through the
-- sso_provider commands.
CREATE TABLE sso_provider (
    id uuid PRIMARY KEY,
    -- Shown on the sign-in page.
    name text NOT NULL CHECK (name <> ''),
    -- Part of the sign-in URLs, and of the redirect URI registered with
    -- the provider.
    slug text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9]([a-z0-9-]{0,62}[a-z0-9])?$'),
    -- Exactly as the provider states it in its discovery document and in
    -- the `iss` claim of its tokens.
    issuer text NOT NULL CHECK (issuer <> ''),
    client_id text NOT NULL CHECK (client_id <> ''),
    scopes text NOT NULL DEFAULT 'openid email profile',
    -- Email domains that may sign in. Empty: any.
    allowed_domains text[] NOT NULL DEFAULT '{}',
    -- Whether a first sign-in creates the user.
    auto_create_users boolean NOT NULL DEFAULT false,
    -- The role a user created at first sign-in is granted, for
    -- default_company_id or, when that is null, for all companies.
    default_role_id uuid REFERENCES role (id),
    default_company_id uuid REFERENCES company (id),
    -- Whether the provider must state that the email address is verified.
    require_verified_email boolean NOT NULL DEFAULT true,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (default_company_id IS NULL OR default_role_id IS NOT NULL)
);

-- The client secret is sent to the provider, so it is kept as given, not
-- hashed. It is in a table of its own, which is not an entity, so that no
-- generic read can return it.
CREATE TABLE sso_provider_secret (
    provider_id uuid PRIMARY KEY REFERENCES sso_provider (id) ON DELETE CASCADE,
    client_secret text NOT NULL CHECK (client_secret <> '')
);

-- A sign-in that was sent to a provider and has not come back yet. The row
-- is deleted when the browser returns, and when it has expired.
CREATE TABLE sso_login_attempt (
    -- SHA-256 of the `state` parameter. The state itself is only in the
    -- browser's address bar.
    state_hash text PRIMARY KEY,
    provider_id uuid NOT NULL REFERENCES sso_provider (id) ON DELETE CASCADE,
    -- Expected back in the ID token. Not a secret: the provider is sent it.
    nonce text NOT NULL,
    -- The PKCE verifier. It is sent to the token endpoint, so it is kept
    -- as it is; it is worth nothing without the authorization code and
    -- the client secret.
    pkce_verifier text NOT NULL,
    -- SHA-256 of the value of the cookie given to the browser that started
    -- the sign-in. Only that browser can finish it.
    browser_hash text NOT NULL,
    -- The local path to go to afterwards.
    return_to text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX sso_login_attempt_expiry ON sso_login_attempt (expires_at);

-- A finished sign-in waiting for the browser to collect its token. Single
-- use: the row is deleted when the code is exchanged, and when it expires.
CREATE TABLE sso_login_code (
    -- SHA-256 of the code.
    code_hash text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES app_user (id) ON DELETE CASCADE,
    provider_id uuid NOT NULL REFERENCES sso_provider (id) ON DELETE CASCADE,
    browser_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX sso_login_code_expiry ON sso_login_code (expires_at);

-- How the token's user signed in: 'password', or 'sso:' and the provider's
-- slug. Null for a token a user created for themselves or for an agent.
ALTER TABLE api_token ADD COLUMN signed_in_with text;

-- Sign-ins and refused sign-ins, for an administrator to look at. Holds no
-- secrets: no password, token, code or state.
CREATE TABLE auth_event (
    id uuid PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT now(),
    -- 'password' or 'sso'.
    kind text NOT NULL CHECK (kind IN ('password', 'sso')),
    -- The address that was given, or that the provider stated. Null when a
    -- sign-in was refused before any was known.
    email text,
    -- The provider's slug. Text, so that the event outlives the provider.
    provider text,
    -- The user, when the address belongs to one.
    user_id uuid REFERENCES app_user (id) ON DELETE SET NULL,
    outcome text NOT NULL CHECK (outcome IN ('success', 'refused')),
    -- Why it was refused.
    reason text
);
CREATE INDEX auth_event_at ON auth_event (at DESC);
