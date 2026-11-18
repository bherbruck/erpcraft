-- Electronic data interchange: trading partners, the log of every
-- interchange received or sent with its bytes, the messages in them, the
-- control numbers, the partners' numbers for items, units and places, and
-- what an order said that its sales order does not keep.

-- The check digit of a GS1 key (GTIN, GLN, SSCC): from the right, the
-- digits are weighted 3, 1, 3, 1 ..., and the last digit brings the sum to
-- a multiple of ten.
CREATE FUNCTION edi_gs1_valid(key text) RETURNS boolean LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    total int := 0;
    n int;
BEGIN
    IF key IS NULL OR key !~ '^[0-9]{2,18}$' THEN
        RETURN false;
    END IF;
    n := length(key) - 1;
    FOR i IN 1..n LOOP
        total := total + substr(key, n - i + 1, 1)::int * CASE WHEN i % 2 = 1 THEN 3 ELSE 1 END;
    END LOOP;
    RETURN (10 - total % 10) % 10 = substr(key, n + 1, 1)::int;
END $$;

-- One separator: a single ASCII character that is not a letter, a digit
-- or a space.
CREATE FUNCTION edi_separator_ok(sep text) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
    SELECT sep IS NULL OR (length(sep) = 1 AND ascii(sep) BETWEEN 33 AND 126
                           AND sep !~ '[A-Za-z0-9]')
$$;

CREATE TABLE edi_partner (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- The customer the partner's orders are for.
    customer_id uuid NOT NULL REFERENCES customer (id),
    -- The warehouse the partner's orders ship from, unless a draft says.
    warehouse_id uuid REFERENCES warehouse (id),
    syntax text NOT NULL DEFAULT 'x12' CHECK (syntax IN ('x12', 'edifact')),
    usage text NOT NULL DEFAULT 'test' CHECK (usage IN ('test', 'production')),
    -- Both sides' interchange identifiers: ISA05 to ISA08, or UNB's.
    our_qualifier text NOT NULL CHECK (our_qualifier ~ '^[A-Z0-9]{1,4}$'),
    our_id text NOT NULL CHECK (our_id ~ '^[ -~]{1,35}$' AND our_id = btrim(our_id)),
    their_qualifier text NOT NULL CHECK (their_qualifier ~ '^[A-Z0-9]{1,4}$'),
    their_id text NOT NULL CHECK (their_id ~ '^[ -~]{1,35}$' AND their_id = btrim(their_id)),
    -- X12's GS02 and GS03; the interchange identifiers when empty.
    our_group_id text CHECK (our_group_id ~ '^[ -~]{1,15}$'),
    their_group_id text CHECK (their_group_id ~ '^[ -~]{1,15}$'),
    version text NOT NULL DEFAULT '004010' CHECK (version IN ('004010', '005010', 'D96A')),
    -- The acknowledgement the partner expects back.
    ack_form text NOT NULL DEFAULT '997' CHECK (ack_form IN ('997', '999', 'contrl', 'none')),
    -- Within how many hours the partner's acknowledgement is due.
    ack_hours int NOT NULL DEFAULT 24 CHECK (ack_hours BETWEEN 1 AND 720),
    -- The separators written to the partner.
    element_separator text NOT NULL DEFAULT '*',
    component_separator text NOT NULL DEFAULT '>',
    repetition_separator text DEFAULT '^',
    segment_terminator text NOT NULL DEFAULT '~',
    -- A line break after each segment written.
    line_breaks boolean NOT NULL DEFAULT false,
    -- The user automation acts as for this partner.
    run_as_user_id uuid REFERENCES app_user (id),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CHECK (edi_separator_ok(element_separator) AND edi_separator_ok(component_separator)
           AND edi_separator_ok(repetition_separator) AND edi_separator_ok(segment_terminator)),
    CHECK (element_separator NOT IN (component_separator, segment_terminator)
           AND component_separator <> segment_terminator
           AND (repetition_separator IS NULL
                OR repetition_separator NOT IN (element_separator, component_separator,
                                                segment_terminator)))
);
-- An interchange finds one partner.
CREATE UNIQUE INDEX edi_partner_route ON edi_partner
    (their_qualifier, their_id, our_qualifier, our_id, usage) WHERE active;

CREATE FUNCTION edi_partner_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.warehouse_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM warehouse WHERE id = NEW.warehouse_id AND company_id = NEW.company_id
    ) THEN
        RAISE EXCEPTION 'the warehouse must belong to the partner''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_partner_check BEFORE INSERT OR UPDATE ON edi_partner
    FOR EACH ROW EXECUTE FUNCTION edi_partner_check();

-- The log of raw payloads, in and out. Written before anything reads the
-- bytes, and never changed or deleted: every message leads back to them.
CREATE TABLE edi_interchange (
    id uuid PRIMARY KEY,
    -- Null until routed; stays null for a file nobody could be found for.
    company_id uuid REFERENCES company (id),
    partner_id uuid REFERENCES edi_partner (id),
    direction text NOT NULL CHECK (direction IN ('in', 'out')),
    syntax text CHECK (syntax IN ('x12', 'edifact')),
    -- ISA13 or UNB's reference, once read.
    control text,
    -- ISA15 as sent: P, T or I.
    usage text,
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size int NOT NULL CHECK (size >= 0),
    payload bytea NOT NULL,
    transport text NOT NULL DEFAULT 'manual' CHECK (transport IN ('manual')),
    transport_ref text,
    state text NOT NULL CHECK (
        (direction = 'in' AND state IN ('stored', 'unreadable', 'unroutable', 'read'))
        OR (direction = 'out' AND state IN ('ready', 'sent', 'failed'))),
    error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz,
    CHECK (octet_length(payload) = size)
);
-- The same bytes received again are a repeat of the first.
CREATE UNIQUE INDEX edi_interchange_repeat ON edi_interchange (sha256) WHERE direction = 'in';
CREATE INDEX edi_interchange_partner ON edi_interchange (partner_id, direction, control);
CREATE INDEX edi_interchange_company ON edi_interchange (company_id, created_at);

CREATE FUNCTION edi_interchange_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'an interchange is never deleted: it is the record of what was exchanged'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.payload IS DISTINCT FROM OLD.payload OR NEW.sha256 IS DISTINCT FROM OLD.sha256
        OR NEW.size IS DISTINCT FROM OLD.size OR NEW.direction IS DISTINCT FROM OLD.direction
        OR (OLD.control IS NOT NULL AND NEW.control IS DISTINCT FROM OLD.control)
        OR (OLD.company_id IS NOT NULL AND NEW.company_id IS DISTINCT FROM OLD.company_id)
        OR (OLD.partner_id IS NOT NULL AND NEW.partner_id IS DISTINCT FROM OLD.partner_id) THEN
        RAISE EXCEPTION 'the bytes of an interchange, their hash, its direction, control number '
                        'and partner never change'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_interchange_guard BEFORE UPDATE OR DELETE ON edi_interchange
    FOR EACH ROW EXECUTE FUNCTION edi_interchange_guard();
CREATE FUNCTION edi_interchange_no_truncate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'interchanges are never deleted' USING ERRCODE = 'check_violation';
END $$;
CREATE TRIGGER edi_interchange_no_truncate BEFORE TRUNCATE ON edi_interchange
    FOR EACH STATEMENT EXECUTE FUNCTION edi_interchange_no_truncate();

-- One transaction set or message, in or out.
CREATE TABLE edi_message (
    id uuid PRIMARY KEY,
    -- Null only for a message to send that failed its checks: no bytes
    -- were fixed for it.
    interchange_id uuid REFERENCES edi_interchange (id),
    company_id uuid NOT NULL REFERENCES company (id),
    partner_id uuid NOT NULL REFERENCES edi_partner (id),
    direction text NOT NULL CHECK (direction IN ('in', 'out')),
    -- The set or message: 850, 855, 856, 810, 997, 999, TA1, ORDERS.
    message text NOT NULL CHECK (message ~ '^[A-Z0-9]{2,6}$'),
    group_control text,
    control text,
    -- Which set of its interchange, counted from 1 over all its groups.
    position int CHECK (position >= 1),
    state text NOT NULL CHECK (
        (direction = 'in' AND state IN ('received', 'rejected', 'test', 'proposed', 'drafted',
                                        'dismissed'))
        OR (direction = 'out' AND state IN ('invalid', 'ready', 'sent', 'acknowledged',
                                            'acknowledged_with_errors', 'rejected', 'failed'))),
    -- The partner's order number, for an order and what answers it.
    business_key text,
    -- BEG01 and the like: original, cancellation, replacement.
    purpose text,
    -- The document the message became or was made from.
    doc_type text,
    document_id uuid REFERENCES document (id),
    -- The message this one answers.
    answers_id uuid REFERENCES edi_message (id),
    -- For an acknowledgement sent (997, 999, TA1): the interchange it
    -- answers.
    answers_interchange_id uuid REFERENCES edi_interchange (id),
    superseded_by uuid REFERENCES edi_message (id),
    -- What reading or checking it found: [{severity, code, line, message}].
    findings jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(findings) = 'array'),
    -- Why it was dismissed.
    reason text,
    -- The partner's acknowledgement of a message sent: A, E or R.
    ack_status text CHECK (ack_status IN ('A', 'E', 'R')),
    -- For a message received: the acknowledgement sent for it. For one
    -- sent: the partner's acknowledgement received.
    ack_message_id uuid REFERENCES edi_message (id),
    ack_due_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (interchange_id, position),
    CHECK ((interchange_id IS NOT NULL AND position IS NOT NULL)
           OR (direction = 'out' AND state = 'invalid'))
);
CREATE INDEX edi_message_partner ON edi_message (partner_id, direction, group_control, control);
CREATE INDEX edi_message_document ON edi_message (doc_type, document_id);
CREATE INDEX edi_message_order ON edi_message (partner_id, business_key) WHERE direction = 'in';
CREATE INDEX edi_message_answers ON edi_message (answers_interchange_id);

CREATE FUNCTION edi_message_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.interchange_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM edi_interchange i
                   WHERE i.id = NEW.interchange_id AND i.direction = NEW.direction
                     AND i.partner_id = NEW.partner_id AND i.company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'a message belongs to an interchange of its partner, company and direction'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.interchange_id IS DISTINCT FROM OLD.interchange_id
        OR NEW.position IS DISTINCT FROM OLD.position OR NEW.message <> OLD.message
        OR NEW.control IS DISTINCT FROM OLD.control
        OR NEW.group_control IS DISTINCT FROM OLD.group_control) THEN
        RAISE EXCEPTION 'a message stays where it was in its interchange'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_message_check BEFORE INSERT OR UPDATE ON edi_message
    FOR EACH ROW EXECUTE FUNCTION edi_message_check();
CREATE FUNCTION edi_message_no_delete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'a message is never deleted' USING ERRCODE = 'check_violation';
END $$;
CREATE TRIGGER edi_message_no_delete BEFORE DELETE ON edi_message
    FOR EACH ROW EXECUTE FUNCTION edi_message_no_delete();

-- The next control number per partner and level, taken in a locked row as
-- the last step before a message's bytes are fixed.
CREATE TABLE edi_control (
    partner_id uuid NOT NULL REFERENCES edi_partner (id),
    kind text NOT NULL CHECK (kind IN ('interchange', 'group', 'set')),
    next_value bigint NOT NULL DEFAULT 1 CHECK (next_value BETWEEN 1 AND 999999999),
    PRIMARY KEY (partner_id, kind)
);

-- The partner's number for an item, or a GTIN for every partner.
CREATE TABLE edi_item_xref (
    id uuid PRIMARY KEY,
    -- Null: every partner (a GTIN).
    partner_id uuid REFERENCES edi_partner (id),
    item_id uuid NOT NULL REFERENCES item (id),
    kind text NOT NULL CHECK (kind IN ('gtin', 'buyer_item', 'seller_item')),
    value text NOT NULL CHECK (value ~ '^[ -~]{1,48}$' AND value = btrim(value)),
    -- The partner's unit for this item, and how many of the item's own unit
    -- one of it is (a case of 6). Null: whatever unit the line names.
    partner_uom text CHECK (partner_uom ~ '^[A-Z0-9]{1,4}$'),
    units_per numeric(20, 6) NOT NULL DEFAULT 1 CHECK (units_per > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (kind <> 'gtin' OR (value ~ '^([0-9]{8}|[0-9]{12,14})$' AND edi_gs1_valid(value))),
    CHECK (partner_id IS NOT NULL OR kind = 'gtin'),
    CHECK (partner_uom IS NOT NULL OR units_per = 1)
);
CREATE UNIQUE INDEX edi_item_xref_value ON edi_item_xref (partner_id, kind, value)
    NULLS NOT DISTINCT;
CREATE INDEX edi_item_xref_item ON edi_item_xref (item_id);

-- The partner's unit codes. Null partner: for every partner.
CREATE TABLE edi_uom_xref (
    id uuid PRIMARY KEY,
    partner_id uuid REFERENCES edi_partner (id),
    partner_uom text NOT NULL CHECK (partner_uom ~ '^[A-Z0-9]{1,4}$'),
    -- The unit as items keep it (`item.uom`).
    uom text NOT NULL CHECK (uom <> ''),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX edi_uom_xref_code ON edi_uom_xref (partner_id, partner_uom) NULLS NOT DISTINCT;

-- The partner's store and warehouse numbers or GLNs, as shipping
-- addresses of its customer. The address is party data, checked by the
-- command that saves it: it is a plain id, as on a document.
CREATE TABLE edi_location_xref (
    id uuid PRIMARY KEY,
    partner_id uuid NOT NULL REFERENCES edi_partner (id),
    -- N103 (or NAD's code list): what kind of number `code` is.
    qualifier text NOT NULL CHECK (qualifier ~ '^[A-Z0-9]{1,3}$'),
    code text NOT NULL CHECK (code ~ '^[ -~]{1,35}$' AND code = btrim(code)),
    address_id uuid NOT NULL,
    -- The warehouse orders to this place ship from, over the partner's.
    warehouse_id uuid REFERENCES warehouse (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (partner_id, qualifier, code)
);
CREATE FUNCTION edi_location_xref_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM edi_partner p
                   JOIN party_address a ON a.party_type = 'customer' AND a.party_id = p.customer_id
                   WHERE p.id = NEW.partner_id AND a.id = NEW.address_id) THEN
        RAISE EXCEPTION 'the address must be one of the addresses of the partner''s customer'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.warehouse_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM warehouse w JOIN edi_partner p ON p.company_id = w.company_id
        WHERE w.id = NEW.warehouse_id AND p.id = NEW.partner_id
    ) THEN
        RAISE EXCEPTION 'the warehouse must belong to the partner''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_location_xref_check BEFORE INSERT OR UPDATE ON edi_location_xref
    FOR EACH ROW EXECUTE FUNCTION edi_location_xref_check();

-- What an order said that its sales order does not keep: the partner's
-- order number and references to quote back.
CREATE TABLE edi_document (
    doc_type text NOT NULL,
    document_id uuid NOT NULL REFERENCES document (id),
    partner_id uuid NOT NULL REFERENCES edi_partner (id),
    message_id uuid NOT NULL REFERENCES edi_message (id),
    order_number text NOT NULL,
    release text,
    order_date date,
    -- References, dates and parties as sent, to echo.
    header jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(header) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (doc_type, document_id)
);
CREATE INDEX edi_document_order ON edi_document (partner_id, order_number);

-- Per line: the partner's line id, item numbers, unit, quantity and price
-- as they were sent, whatever the cross-references say later.
CREATE TABLE edi_document_line (
    doc_type text NOT NULL,
    document_id uuid NOT NULL,
    -- The document's own line.
    line_id uuid NOT NULL,
    partner_line_id text NOT NULL,
    -- [{qualifier, value}], in the order sent.
    product_ids jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(product_ids) = 'array'),
    partner_uom text NOT NULL,
    partner_qty numeric(20, 6) NOT NULL,
    partner_price numeric(20, 6),
    -- How many of the item's own unit one partner unit is.
    units_per numeric(20, 6) NOT NULL DEFAULT 1 CHECK (units_per > 0),
    PRIMARY KEY (doc_type, document_id, line_id),
    FOREIGN KEY (doc_type, document_id) REFERENCES edi_document (doc_type, document_id)
);
