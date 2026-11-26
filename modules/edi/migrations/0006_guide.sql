-- A partner's implementation guide as data (PLAN.md, phase 4). A guide
-- is the company's, for one syntax and version; it may build on one other
-- guide of the company (one level), and every guide builds on the
-- module's own base for its syntax and version (guides/*.json, in the
-- code). What it says about each message is a spec of three closed parts
-- (options, constraints, extra), checked when it is written.
CREATE TABLE edi_guide (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code ~ '^[A-Za-z0-9_.-]{1,40}$'),
    name text NOT NULL CHECK (name <> ''),
    syntax text NOT NULL CHECK (syntax IN ('x12', 'edifact')),
    version text NOT NULL CHECK (version IN ('004010', '005010', 'D96A')),
    base_guide_id uuid REFERENCES edi_guide (id),
    notes text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CHECK ((syntax = 'edifact') = (version = 'D96A')),
    CHECK (base_guide_id IS DISTINCT FROM id)
);

CREATE TABLE edi_guide_message (
    id uuid PRIMARY KEY,
    -- The guide's, so the message is read and written with the company's
    -- rights.
    company_id uuid NOT NULL REFERENCES company (id),
    guide_id uuid NOT NULL REFERENCES edi_guide (id) ON DELETE CASCADE,
    message text NOT NULL,
    direction text NOT NULL CHECK (direction IN ('in', 'out')),
    spec jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(spec) = 'object'),
    UNIQUE (guide_id, message, direction)
);

ALTER TABLE edi_partner ADD COLUMN guide_id uuid REFERENCES edi_guide (id);

-- A base is a guide of the same company, syntax and version that has no
-- base of its own.
CREATE FUNCTION edi_guide_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.base_guide_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM edi_guide b
        WHERE b.id = NEW.base_guide_id AND b.company_id = NEW.company_id
          AND b.syntax = NEW.syntax AND b.version = NEW.version AND b.base_guide_id IS NULL
    ) THEN
        RAISE EXCEPTION 'the base guide must be a guide of the same company, syntax and version that has no base of its own'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND NEW.base_guide_id IS NOT NULL
       AND EXISTS (SELECT 1 FROM edi_guide WHERE base_guide_id = NEW.id) THEN
        RAISE EXCEPTION 'a guide other guides build on cannot have a base of its own'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.syntax, NEW.version) IS DISTINCT FROM (OLD.syntax, OLD.version)
       AND (EXISTS (SELECT 1 FROM edi_guide_message WHERE guide_id = NEW.id)
            OR EXISTS (SELECT 1 FROM edi_partner WHERE guide_id = NEW.id)
            OR EXISTS (SELECT 1 FROM edi_guide WHERE base_guide_id = NEW.id)) THEN
        RAISE EXCEPTION 'the syntax and version of a guide in use cannot change'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_guide_check BEFORE INSERT OR UPDATE ON edi_guide
    FOR EACH ROW EXECUTE FUNCTION edi_guide_check();

-- A message of the guide's syntax, in the direction it travels.
CREATE FUNCTION edi_guide_message_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    guide_syntax text;
    guide_company uuid;
BEGIN
    SELECT syntax, company_id INTO guide_syntax, guide_company FROM edi_guide WHERE id = NEW.guide_id;
    IF guide_company IS DISTINCT FROM NEW.company_id THEN
        RAISE EXCEPTION 'the message must be of a guide of its company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT (
        (guide_syntax = 'x12' AND (NEW.message, NEW.direction) IN
            (('850', 'in'), ('855', 'out'), ('856', 'out'), ('810', 'out')))
        OR (guide_syntax = 'edifact' AND (NEW.message, NEW.direction) IN
            (('ORDERS', 'in'), ('ORDRSP', 'out'), ('DESADV', 'out'), ('INVOIC', 'out')))
    ) THEN
        RAISE EXCEPTION 'message % (%) is not one a guide of this syntax describes', NEW.message, NEW.direction
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_guide_message_check BEFORE INSERT OR UPDATE ON edi_guide_message
    FOR EACH ROW EXECUTE FUNCTION edi_guide_message_check();

-- A partner follows a guide of its company, syntax and version.
CREATE FUNCTION edi_partner_guide_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.guide_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM edi_guide g
        WHERE g.id = NEW.guide_id AND g.company_id = NEW.company_id
          AND g.syntax = NEW.syntax AND g.version = NEW.version
    ) THEN
        RAISE EXCEPTION 'the guide must be one of the partner''s company, for its syntax and version'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_partner_guide_check BEFORE INSERT OR UPDATE OF guide_id, syntax, version, company_id
    ON edi_partner FOR EACH ROW EXECUTE FUNCTION edi_partner_guide_check();
