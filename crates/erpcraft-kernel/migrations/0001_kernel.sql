-- Kernel schema: currencies, companies, the user directory, authorization,
-- audit, outbox, idempotency, numbering, documents, postings.

CREATE TABLE currency (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code ~ '^[A-Z]{3}$'),
    name text NOT NULL CHECK (name <> ''),
    -- Minor units. Amounts in this currency never carry more decimals.
    decimals smallint NOT NULL DEFAULT 2 CHECK (decimals BETWEEN 0 AND 4),
    created_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO currency (id, code, name, decimals) VALUES
    (gen_random_uuid(), 'USD', 'US Dollar', 2),
    (gen_random_uuid(), 'EUR', 'Euro', 2),
    (gen_random_uuid(), 'GBP', 'Pound Sterling', 2),
    (gen_random_uuid(), 'CAD', 'Canadian Dollar', 2),
    (gen_random_uuid(), 'MXN', 'Mexican Peso', 2),
    (gen_random_uuid(), 'CHF', 'Swiss Franc', 2),
    (gen_random_uuid(), 'AUD', 'Australian Dollar', 2),
    (gen_random_uuid(), 'CNY', 'Chinese Yuan', 2),
    (gen_random_uuid(), 'INR', 'Indian Rupee', 2),
    (gen_random_uuid(), 'JPY', 'Japanese Yen', 0);

CREATE TABLE company (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- The accounting currency. Every ledger balances in it.
    base_currency text NOT NULL REFERENCES currency (code),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The directory of users. Credentials are not kept here: they belong to
-- whichever module authenticates users.
CREATE TABLE app_user (
    id uuid PRIMARY KEY,
    email text NOT NULL UNIQUE CHECK (email <> ''),
    name text NOT NULL,
    is_admin boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    -- Facts about the user that permission rules can refer to, such as
    -- {"warehouses": ["W1", "W2"]}.
    attributes jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(attributes) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE role (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (name <> ''),
    -- A list of permission rules. Each is "resource:action", or an object
    -- that allows or denies one, optionally for some records and fields.
    permissions jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(permissions) = 'array'),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A role is granted for one company, or for all companies when company_id
-- is NULL.
CREATE TABLE role_assignment (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES app_user (id) ON DELETE CASCADE,
    role_id uuid NOT NULL REFERENCES role (id),
    company_id uuid REFERENCES company (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (user_id, role_id, company_id)
);

CREATE FUNCTION erp_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% records are immutable', TG_TABLE_NAME USING ERRCODE = 'check_violation';
END $$;

CREATE TABLE audit_log (
    id uuid PRIMARY KEY,
    at timestamptz NOT NULL DEFAULT now(),
    user_id uuid NOT NULL REFERENCES app_user (id),
    command text NOT NULL,
    -- NULL for commands whose input is sensitive.
    input jsonb
);
CREATE INDEX audit_log_at ON audit_log (at);
CREATE TRIGGER audit_log_immutable BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER audit_log_no_truncate BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- Written in the same transaction as the change it describes. A delivery
-- worker publishes rows after commit and stamps published_at.
CREATE TABLE outbox_event (
    id uuid PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now(),
    topic text NOT NULL,
    payload jsonb NOT NULL,
    published_at timestamptz
);
CREATE INDEX outbox_event_unpublished ON outbox_event (created_at) WHERE published_at IS NULL;

CREATE TABLE idempotency_key (
    user_id uuid NOT NULL REFERENCES app_user (id) ON DELETE CASCADE,
    key text NOT NULL,
    command text NOT NULL,
    input_hash text NOT NULL,
    output jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, key)
);

CREATE TABLE number_series (
    company_id uuid NOT NULL REFERENCES company (id),
    series text NOT NULL,
    last_value bigint NOT NULL,
    PRIMARY KEY (company_id, series)
);

-- A business document. Its states and transitions are defined by the
-- module that owns the document type. "locked" mirrors the current state:
-- a locked document's content cannot change.
CREATE TABLE document (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    doc_type text NOT NULL,
    number text,
    status text NOT NULL,
    locked boolean NOT NULL DEFAULT false,
    -- Incremented on every change; callers send it back to detect stale edits.
    version integer NOT NULL DEFAULT 1,
    -- When the business event happened.
    doc_date date NOT NULL,
    memo text,
    created_by uuid NOT NULL REFERENCES app_user (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, doc_type, number)
);
CREATE INDEX document_type_company ON document (doc_type, company_id, created_at);

CREATE FUNCTION erp_document_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.locked THEN
            RAISE EXCEPTION 'document % is % and cannot be deleted', OLD.id, OLD.status
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN OLD;
    END IF;
    IF NEW.company_id <> OLD.company_id OR NEW.doc_type <> OLD.doc_type THEN
        RAISE EXCEPTION 'the company and type of a document cannot change'
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.number IS NOT NULL AND NEW.number IS DISTINCT FROM OLD.number THEN
        RAISE EXCEPTION 'the number of document % cannot change', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.locked AND (NEW.doc_date <> OLD.doc_date OR NEW.memo IS DISTINCT FROM OLD.memo) THEN
        RAISE EXCEPTION 'document % is % and can no longer be changed', OLD.id, OLD.status
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER document_guard BEFORE UPDATE OR DELETE ON document
    FOR EACH ROW EXECUTE FUNCTION erp_document_guard();

-- Modules attach this to every table holding document content. The table
-- must have a document_id column.
CREATE FUNCTION erp_document_child_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    doc document%ROWTYPE;
BEGIN
    IF TG_OP <> 'INSERT' THEN
        SELECT * INTO doc FROM document WHERE id = OLD.document_id;
        -- A missing parent means the delete is cascading from a draft.
        IF FOUND AND doc.locked THEN
            RAISE EXCEPTION 'document % is % and can no longer be changed', doc.id, doc.status
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    IF TG_OP <> 'DELETE' THEN
        SELECT * INTO doc FROM document WHERE id = NEW.document_id;
        IF FOUND AND doc.locked THEN
            RAISE EXCEPTION 'document % is % and can no longer be changed', doc.id, doc.status
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    RETURN OLD;
END $$;

-- An immutable record that a document changed the books. Ledger entries
-- hang off a posting. One document may have many postings. A posting is
-- never edited: it is undone by a second posting that reverses it.
CREATE TABLE posting (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    document_id uuid NOT NULL REFERENCES document (id),
    -- The command that created the posting.
    command text NOT NULL,
    -- The accounting date. created_at is when the system recorded it.
    posting_date date NOT NULL,
    memo text,
    -- UNIQUE: a posting can be reversed at most once.
    reverses_id uuid UNIQUE REFERENCES posting (id),
    created_by uuid NOT NULL REFERENCES app_user (id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX posting_document ON posting (document_id);

CREATE FUNCTION erp_posting_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    doc_company uuid;
    original posting%ROWTYPE;
BEGIN
    SELECT company_id INTO doc_company FROM document WHERE id = NEW.document_id;
    IF doc_company <> NEW.company_id THEN
        RAISE EXCEPTION 'a posting must belong to the company of its document'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO original FROM posting WHERE id = NEW.reverses_id;
        IF original.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        IF original.company_id <> NEW.company_id OR original.document_id <> NEW.document_id THEN
            RAISE EXCEPTION 'a reversal must belong to the document it reverses'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER posting_before_insert BEFORE INSERT ON posting
    FOR EACH ROW EXECUTE FUNCTION erp_posting_before_insert();
CREATE TRIGGER posting_immutable BEFORE UPDATE OR DELETE ON posting
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER posting_no_truncate BEFORE TRUNCATE ON posting
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
