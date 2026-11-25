-- Price lists: the price and discount an order line is proposed. See
-- "Price lists" in ARCHITECTURE.md.

-- A group of customers, vendors or items, shared by companies.
CREATE TABLE price_group (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('customer', 'vendor', 'item')),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (kind, code)
);

-- A customer, vendor or item in a group of its kind: at most one. The
-- records are other modules': checked by the trigger, as tax rules name
-- their scope, and removed with them by nothing (a member that is gone
-- matches nothing).
CREATE TABLE price_group_member (
    id uuid PRIMARY KEY,
    group_id uuid NOT NULL REFERENCES price_group (id) ON DELETE CASCADE,
    kind text NOT NULL,
    member_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (kind, member_id)
);

CREATE FUNCTION price_group_member_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    found boolean;
BEGIN
    NEW.kind := (SELECT kind FROM price_group WHERE id = NEW.group_id);
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I WHERE id = $1)', NEW.kind)
        INTO found USING NEW.member_id;
    IF NOT found THEN
        RAISE EXCEPTION 'the % of a price group member does not exist', NEW.kind
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER price_group_member_check BEFORE INSERT OR UPDATE ON price_group_member
    FOR EACH ROW EXECUTE FUNCTION price_group_member_check();

CREATE TABLE price_list (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    side text NOT NULL CHECK (side IN ('sales', 'purchase')),
    currency text NOT NULL REFERENCES currency (code),
    -- Whom it is for; none: everyone on its side.
    scope_type text CHECK (scope_type IN ('customer', 'customer_group', 'vendor', 'vendor_group')),
    scope_id uuid,
    valid_from date,
    valid_to date,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CONSTRAINT price_list_scope_whole CHECK ((scope_type IS NULL) = (scope_id IS NULL)),
    CONSTRAINT price_list_scope_side CHECK (
        scope_type IS NULL
        OR (side = 'sales' AND scope_type IN ('customer', 'customer_group'))
        OR (side = 'purchase' AND scope_type IN ('vendor', 'vendor_group'))),
    CONSTRAINT price_list_dates CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to >= valid_from)
);
CREATE INDEX price_list_lookup ON price_list (company_id, side, currency) WHERE active;

CREATE FUNCTION price_list_scope_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    found boolean;
BEGIN
    IF NEW.scope_type IN ('customer', 'vendor') THEN
        EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I WHERE id = $1)', NEW.scope_type)
            INTO found USING NEW.scope_id;
    ELSIF NEW.scope_type IS NOT NULL THEN
        found := EXISTS (SELECT 1 FROM price_group
                         WHERE id = NEW.scope_id AND kind || '_group' = NEW.scope_type);
    ELSE
        found := true;
    END IF;
    IF NOT found THEN
        RAISE EXCEPTION 'the price list is for a % that does not exist', NEW.scope_type
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER price_list_scope_check BEFORE INSERT OR UPDATE ON price_list
    FOR EACH ROW EXECUTE FUNCTION price_list_scope_check();

CREATE TABLE price_list_line (
    id uuid PRIMARY KEY,
    price_list_id uuid NOT NULL REFERENCES price_list (id) ON DELETE CASCADE,
    item_id uuid REFERENCES item (id),
    item_group_id uuid REFERENCES price_group (id),
    -- A quantity break: from this quantity on.
    min_qty numeric(20, 6) NOT NULL DEFAULT 0 CHECK (min_qty >= 0),
    unit_price numeric(20, 6) CHECK (unit_price >= 0),
    discount_percent numeric(9, 6) CHECK (discount_percent > 0 AND discount_percent <= 100),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT price_list_line_for CHECK ((item_id IS NULL) <> (item_group_id IS NULL)),
    CONSTRAINT price_list_line_says CHECK (unit_price IS NOT NULL OR discount_percent IS NOT NULL),
    CONSTRAINT price_list_line_once UNIQUE NULLS NOT DISTINCT
        (price_list_id, item_id, item_group_id, min_qty)
);
CREATE INDEX price_list_line_item ON price_list_line (item_id);

CREATE FUNCTION price_list_line_group_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.item_group_id IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM price_group WHERE id = NEW.item_group_id AND kind = 'item') THEN
        RAISE EXCEPTION 'a price list line is for an item or a group of items'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER price_list_line_group_check BEFORE INSERT OR UPDATE ON price_list_line
    FOR EACH ROW EXECUTE FUNCTION price_list_line_group_check();

-- What an order line was proposed, kept beside the order so that approving
-- it can tell a price that was changed.
CREATE TABLE price_proposal (
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_id uuid NOT NULL,
    unit_price numeric(20, 6),
    discount_percent numeric(9, 6),
    -- list or last_paid.
    source text NOT NULL CHECK (source IN ('list', 'last_paid')),
    -- The list line it came from; not a key, as lists change after orders.
    price_list_line_id uuid,
    PRIMARY KEY (document_id, line_id)
);
CREATE TRIGGER price_proposal_guard BEFORE INSERT OR UPDATE OR DELETE ON price_proposal
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
