-- Single sign-on, after review: users are bound to the provider they sign
-- in through, a provider cannot be active without its secret, the
-- browser's cookie has one exact name per sign-in, and changes to
-- providers and bindings are on record.

-- Whether administrators may sign in through the provider. A provider
-- that can assert an administrator's identity holds administrator rights.
ALTER TABLE sso_provider ADD COLUMN trusted_for_admins boolean NOT NULL DEFAULT false;

-- allowed_domains can no longer be left empty to mean "any": that is said
-- with the single entry '*'. A provider saved before this, with none,
-- signs nobody in until its domains are set.

-- The identity at a provider that a user signs in with: the provider and
-- the subject (`sub`) it states. A user has at most one. It is set by the
-- first sign-in that is allowed to, or by an administrator; after that
-- the user is accepted only from this provider under this subject, and an
-- email address alone is not enough.
CREATE TABLE sso_identity (
    user_id uuid PRIMARY KEY REFERENCES app_user (id) ON DELETE CASCADE,
    provider_id uuid NOT NULL REFERENCES sso_provider (id) ON DELETE CASCADE,
    subject text NOT NULL CHECK (subject <> ''),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (provider_id, subject)
);

-- The name of the cookie that ties a sign-in to its browser. Rows from
-- before this have none and cannot be finished.
ALTER TABLE sso_login_attempt ADD COLUMN cookie_id text;
ALTER TABLE sso_login_code ADD COLUMN cookie_id text;

-- The subject the provider stated, so that an administrator can bind a
-- user to it. And two more kinds of event: a change to a provider, and a
-- change to a user's binding. For those, user_id and email are the person
-- who made the change, and reason says what changed.
ALTER TABLE auth_event ADD COLUMN subject text;
ALTER TABLE auth_event DROP CONSTRAINT auth_event_kind_check;
ALTER TABLE auth_event ADD CONSTRAINT auth_event_kind_check
    CHECK (kind IN ('password', 'sso', 'sso_provider', 'sso_identity'));

-- An active provider has a client secret. Checked when the transaction
-- commits, so that a provider and its secret can be written in either
-- order within one.
CREATE FUNCTION sso_provider_needs_secret() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    checked uuid;
BEGIN
    IF TG_TABLE_NAME = 'sso_provider' THEN
        checked := NEW.id;
    ELSE
        checked := OLD.provider_id;
    END IF;
    IF EXISTS (SELECT 1 FROM sso_provider p WHERE p.id = checked AND p.active
               AND NOT EXISTS (SELECT 1 FROM sso_provider_secret s WHERE s.provider_id = p.id)) THEN
        RAISE EXCEPTION 'a single sign-on provider without a client secret cannot be active'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER sso_provider_needs_secret AFTER INSERT OR UPDATE ON sso_provider
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sso_provider_needs_secret();
CREATE CONSTRAINT TRIGGER sso_secret_needed AFTER DELETE ON sso_provider_secret
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sso_provider_needs_secret();
