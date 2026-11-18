-- Account rules: which account a company posts to for a purpose, such as
-- `inventory`, optionally depending on what the posting is about.
--
-- A rule with a scope applies to one record of some other module: an item,
-- a warehouse. A rule without one is the company's default for the
-- purpose. The ledger does not know what a scope is; it only matches it.

CREATE TABLE account_rule (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    purpose text NOT NULL CHECK (purpose ~ '^[a-z][a-z0-9_]*$'),
    -- The entity the rule is limited to, and which record of it. Both or
    -- neither.
    scope_type text CHECK (scope_type ~ '^[a-z][a-z0-9_]*$'),
    scope_id uuid,
    account_id uuid NOT NULL REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT account_rule_scope_whole CHECK ((scope_type IS NULL) = (scope_id IS NULL)),
    -- One answer per question: two defaults for a purpose are a duplicate too.
    CONSTRAINT account_rule_once UNIQUE NULLS NOT DISTINCT (company_id, purpose, scope_type, scope_id)
);

CREATE FUNCTION gl_account_rule_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    acc account%ROWTYPE;
BEGIN
    SELECT * INTO acc FROM account WHERE id = NEW.account_id;
    IF acc.company_id IS DISTINCT FROM NEW.company_id THEN
        RAISE EXCEPTION 'the account of an account rule must belong to the rule''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF acc.is_group THEN
        RAISE EXCEPTION 'account % is a group account and cannot be posted to', acc.code
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER account_rule_check BEFORE INSERT OR UPDATE ON account_rule
    FOR EACH ROW EXECUTE FUNCTION gl_account_rule_check();
