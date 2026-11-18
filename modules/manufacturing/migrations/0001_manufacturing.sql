-- Manufacturing: a bill of materials says what a product is made of, and a
-- production order makes it.
--
-- Completing an order takes the components out of stock at their average
-- cost and brings the product in for exactly the value that left. In the
-- general ledger the same value moves from the materials account to the
-- finished goods account.

-- Bills of materials are shared by all companies, as items are.
CREATE TABLE bom (
    id uuid PRIMARY KEY,
    -- The product this bill makes.
    item_id uuid NOT NULL REFERENCES item (id),
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- How many units of the product one run of this bill makes.
    output_qty numeric(20, 6) NOT NULL DEFAULT 1 CHECK (output_qty > 0),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE bom_line (
    id uuid PRIMARY KEY,
    bom_id uuid NOT NULL REFERENCES bom (id) ON DELETE CASCADE,
    -- A component.
    item_id uuid NOT NULL REFERENCES item (id),
    -- Needed for one run of the bill.
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bom_id, item_id)
);

-- The product of a bill is fixed, so checking each line as it is written
-- is enough to keep a product out of its own bill.
CREATE FUNCTION bom_product_fixed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.item_id <> OLD.item_id THEN
        RAISE EXCEPTION 'the product of a bill of materials cannot change; create a new bill instead'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bom_product_fixed BEFORE UPDATE ON bom
    FOR EACH ROW EXECUTE FUNCTION bom_product_fixed();

CREATE FUNCTION bom_line_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.bom_id <> OLD.bom_id THEN
        RAISE EXCEPTION 'a component cannot move to another bill of materials'
            USING ERRCODE = 'check_violation';
    END IF;
    -- The share lock keeps the bill from being deleted meanwhile.
    IF NEW.item_id = (SELECT item_id FROM bom WHERE id = NEW.bom_id FOR SHARE) THEN
        RAISE EXCEPTION 'a bill of materials cannot list its own product as a component'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bom_line_check BEFORE INSERT OR UPDATE ON bom_line
    FOR EACH ROW EXECUTE FUNCTION bom_line_check();

CREATE TABLE manufacturing_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    -- The asset account that carries the value of components.
    materials_account_id uuid NOT NULL REFERENCES account (id),
    -- The asset account that carries the value of what was made.
    finished_goods_account_id uuid NOT NULL REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION manufacturing_settings_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM account
        WHERE company_id = NEW.company_id
          AND id IN (NEW.materials_account_id, NEW.finished_goods_account_id))
       <> (SELECT count(DISTINCT a)
           FROM unnest(ARRAY[NEW.materials_account_id, NEW.finished_goods_account_id]) AS a) THEN
        RAISE EXCEPTION 'the manufacturing accounts must belong to the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER manufacturing_settings_check BEFORE INSERT OR UPDATE ON manufacturing_settings
    FOR EACH ROW EXECUTE FUNCTION manufacturing_settings_check();

-- ---------------------------------------------------------- production order

CREATE TABLE production_order (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    bom_id uuid NOT NULL REFERENCES bom (id),
    -- Units of the product to make.
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- The components are taken from it and the product goes into it.
    warehouse_id uuid NOT NULL REFERENCES warehouse (id)
);

-- What the order consumes: the bill's lines as they were when the order
-- was released, scaled to the order quantity. A later change to the bill
-- does not reach a released order.
CREATE TABLE production_order_component (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, item_id)
);

CREATE TRIGGER production_order_guard BEFORE INSERT OR UPDATE OR DELETE ON production_order
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER production_order_component_guard BEFORE INSERT OR UPDATE OR DELETE ON production_order_component
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
