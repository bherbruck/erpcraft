-- Credentials. The users themselves live in the kernel's app_user table.

CREATE TABLE user_password (
    user_id uuid PRIMARY KEY REFERENCES app_user (id) ON DELETE CASCADE,
    password_hash text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE api_token (
    id uuid PRIMARY KEY,
    -- Only a hash is stored; the token itself is shown once.
    token_hash text NOT NULL UNIQUE,
    user_id uuid NOT NULL REFERENCES app_user (id) ON DELETE CASCADE,
    name text NOT NULL,
    -- NULL means the token carries all of its user's rights. Otherwise the
    -- token is limited to permissions matching one of these patterns.
    scopes text[],
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX api_token_user ON api_token (user_id);
