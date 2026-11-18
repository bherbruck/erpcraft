-- Dimensions: what an entry is about besides its account, such as the site,
-- the cost center or the project.
--
-- A company defines its dimensions and their values. Every ledger entry
-- points to a dimension set: one value for some of the company's
-- dimensions. Sets are immutable and kept once per combination, so the same
-- values always give the same set; an entry without dimensions has no set
-- (NULL), which is the empty set. Which values an entry gets by default is
-- said by dimension rules, matched like account rules: for a record of
-- another module (a warehouse, a site, an item, a party, an account), or for
-- the company.

CREATE TABLE dimension (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- Shown in a set as CODE:VALUE, so neither may hold ':' or ','.
    code text NOT NULL CHECK (code <> '' AND code !~ '[:,]'),
    name text NOT NULL CHECK (name <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code)
);

CREATE TABLE dimension_value (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    dimension_id uuid NOT NULL REFERENCES dimension (id),
    code text NOT NULL CHECK (code <> '' AND code !~ '[:,]'),
    name text NOT NULL CHECK (name <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (dimension_id, code),
    -- The target of foreign keys that also name the dimension.
    UNIQUE (id, dimension_id)
);

-- A set's label is made of the codes, so codes do not change once used,
-- and a value stays with its dimension and company.
CREATE FUNCTION gl_dimension_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'dimension' THEN
        IF TG_OP = 'UPDATE' AND (NEW.code <> OLD.code OR NEW.company_id <> OLD.company_id) THEN
            RAISE EXCEPTION 'the code and company of a dimension do not change'
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM dimension
                   WHERE id = NEW.dimension_id AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'a dimension value must belong to the company of its dimension'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.code <> OLD.code OR NEW.dimension_id <> OLD.dimension_id) THEN
        RAISE EXCEPTION 'the code and dimension of a dimension value do not change'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER dimension_check BEFORE INSERT OR UPDATE ON dimension
    FOR EACH ROW EXECUTE FUNCTION gl_dimension_check();
CREATE TRIGGER dimension_value_check BEFORE INSERT OR UPDATE ON dimension_value
    FOR EACH ROW EXECUTE FUNCTION gl_dimension_check();

-- One combination of values. `key` is the ids of its values, sorted and
-- joined, which makes the combination unique per company; `label` is what a
-- person reads: DIMENSION:VALUE, by dimension code.
CREATE TABLE dimension_set (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    key text NOT NULL CHECK (key <> ''),
    label text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, key),
    UNIQUE (id, company_id)
);

CREATE TABLE dimension_set_value (
    set_id uuid NOT NULL REFERENCES dimension_set (id),
    dimension_id uuid NOT NULL REFERENCES dimension (id),
    value_id uuid NOT NULL,
    -- One value per dimension.
    PRIMARY KEY (set_id, dimension_id),
    FOREIGN KEY (value_id, dimension_id) REFERENCES dimension_value (id, dimension_id)
);
CREATE INDEX dimension_set_value_value ON dimension_set_value (value_id);

CREATE TRIGGER dimension_set_immutable BEFORE UPDATE OR DELETE ON dimension_set
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER dimension_set_no_truncate BEFORE TRUNCATE ON dimension_set
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER dimension_set_value_immutable BEFORE UPDATE OR DELETE ON dimension_set_value
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER dimension_set_value_no_truncate BEFORE TRUNCATE ON dimension_set_value
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- The set of the given values, created the first time the combination is
-- used. The values must belong to the company, one per dimension; an empty
-- list is the empty set, NULL. Two transactions creating the same set wait
-- for each other on the unique key and get the same id.
CREATE FUNCTION dimension_set_of(company uuid, value_ids uuid[]) RETURNS uuid
LANGUAGE plpgsql AS $$
DECLARE
    set_key text;
    set_label text;
    set_id uuid;
    wanted integer;
    known integer;
    dims integer;
BEGIN
    SELECT string_agg(DISTINCT v::text, ',' ORDER BY v::text) INTO set_key
        FROM unnest(value_ids) AS v WHERE v IS NOT NULL;
    IF set_key IS NULL THEN
        RETURN NULL;
    END IF;
    SELECT id INTO set_id FROM dimension_set WHERE company_id = company AND key = set_key;
    IF set_id IS NOT NULL THEN
        RETURN set_id;
    END IF;
    SELECT count(DISTINCT v) INTO wanted FROM unnest(value_ids) AS v WHERE v IS NOT NULL;
    SELECT count(*), count(DISTINCT dv.dimension_id),
           string_agg(d.code || ':' || dv.code, ', ' ORDER BY d.code)
        INTO known, dims, set_label
        FROM dimension_value dv JOIN dimension d ON d.id = dv.dimension_id
        WHERE dv.id = ANY (value_ids) AND dv.company_id = company;
    IF known <> wanted THEN
        RAISE EXCEPTION 'a dimension value of the set belongs to another company or does not exist'
            USING ERRCODE = 'check_violation';
    END IF;
    IF dims <> known THEN
        RAISE EXCEPTION 'a dimension set holds one value per dimension'
            USING ERRCODE = 'check_violation';
    END IF;
    set_id := gen_random_uuid();
    INSERT INTO dimension_set (id, company_id, key, label)
        VALUES (set_id, company, set_key, set_label)
        ON CONFLICT (company_id, key) DO NOTHING;
    IF NOT FOUND THEN
        SELECT id INTO set_id FROM dimension_set WHERE company_id = company AND key = set_key;
        RETURN set_id;
    END IF;
    INSERT INTO dimension_set_value (set_id, dimension_id, value_id)
        SELECT set_id, dv.dimension_id, dv.id FROM dimension_value dv WHERE dv.id = ANY (value_ids);
    RETURN set_id;
END $$;

-- The set of each ledger entry. Entries written before dimensions existed
-- have none. The set must be the entry's company's.
ALTER TABLE gl_entry ADD COLUMN dimension_set_id uuid;
ALTER TABLE gl_entry ADD CONSTRAINT gl_entry_dimension_set
    FOREIGN KEY (dimension_set_id, company_id) REFERENCES dimension_set (id, company_id);
CREATE INDEX gl_entry_dimension_set ON gl_entry (dimension_set_id)
    WHERE dimension_set_id IS NOT NULL;

-- Which value an entry gets by default, per dimension: for one record of
-- another module (scope_type names its entity: warehouse, site, item,
-- customer, vendor, account, ...), or, without a scope, for the company.
-- The ledger does not know what a scope is; it only matches it.
CREATE TABLE dimension_rule (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- Set from the value.
    dimension_id uuid NOT NULL REFERENCES dimension (id),
    scope_type text CHECK (scope_type ~ '^[a-z][a-z0-9_]*$'),
    scope_id uuid,
    value_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT dimension_rule_scope_whole CHECK ((scope_type IS NULL) = (scope_id IS NULL)),
    CONSTRAINT dimension_rule_once
        UNIQUE NULLS NOT DISTINCT (company_id, dimension_id, scope_type, scope_id),
    FOREIGN KEY (value_id, dimension_id) REFERENCES dimension_value (id, dimension_id)
);
CREATE INDEX dimension_rule_scope ON dimension_rule (scope_type, scope_id);

CREATE FUNCTION gl_dimension_rule_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    value_company uuid;
BEGIN
    SELECT company_id, dimension_id INTO value_company, NEW.dimension_id
        FROM dimension_value WHERE id = NEW.value_id;
    IF value_company IS DISTINCT FROM NEW.company_id THEN
        RAISE EXCEPTION 'the value of a dimension rule must belong to the rule''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER dimension_rule_check BEFORE INSERT OR UPDATE ON dimension_rule
    FOR EACH ROW EXECUTE FUNCTION gl_dimension_rule_check();
