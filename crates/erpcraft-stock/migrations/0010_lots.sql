-- Lots and serial numbers.
--
-- An item is tracked by lot or by serial number, or not at all. A serial
-- number is a lot that is never more than one unit. A stock entry stays
-- one per document line and is valued as before; the lots it moved are
-- rows of stock_entry_lot. Lots do not change costing: the moving average
-- stays per item and warehouse.
--
-- The invariants, enforced here:
-- - an entry of a tracked item carries lots adding up to its quantity, the
--   way it goes; an entry of an untracked item, and a cost adjustment,
--   carries none (checked at commit: an entry and its lots are written in
--   one transaction, and lots are never added to an entry later);
-- - a lot level never goes below zero, so a tracked item never does;
-- - a serial number is on hand at most once in its company, one unit;
-- - a reversal takes back exactly the lots of the entry it reverses;
-- - tracking changes only while the item has no stock anywhere.

ALTER TABLE item ADD COLUMN tracking text NOT NULL DEFAULT 'none'
    CHECK (tracking IN ('none', 'lot', 'serial'));

CREATE TABLE stock_lot (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    item_id uuid NOT NULL REFERENCES item (id),
    -- The lot or serial number.
    code text NOT NULL CHECK (code <> '' AND code = btrim(code) AND length(code) <= 64),
    expiry_date date,
    manufactured_date date,
    -- Free attributes: a supplier's lot number, a grade, a certificate.
    attributes jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(attributes) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, item_id, code),
    CHECK (expiry_date IS NULL OR manufactured_date IS NULL OR expiry_date >= manufactured_date)
);
CREATE INDEX stock_lot_item ON stock_lot (item_id, company_id);
CREATE INDEX stock_lot_code_trgm ON stock_lot USING gin (code gin_trgm_ops);

-- A lot's code, item and company never change.
CREATE FUNCTION stock_lot_fixed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code <> OLD.code OR NEW.item_id <> OLD.item_id OR NEW.company_id <> OLD.company_id THEN
        RAISE EXCEPTION 'a lot''s code, item and company do not change'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_lot_fixed BEFORE UPDATE ON stock_lot
    FOR EACH ROW EXECUTE FUNCTION stock_lot_fixed();

-- The quantity of a lot in a warehouse: the running total of
-- stock_entry_lot, kept by its trigger.
CREATE TABLE stock_lot_level (
    lot_id uuid NOT NULL REFERENCES stock_lot (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    item_id uuid NOT NULL REFERENCES item (id),
    company_id uuid NOT NULL REFERENCES company (id),
    qty numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty >= 0),
    PRIMARY KEY (lot_id, warehouse_id)
);
CREATE INDEX stock_lot_level_item ON stock_lot_level (item_id, warehouse_id) WHERE qty > 0;

CREATE FUNCTION stock_lot_level_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.qty = 0 THEN
        RETURN NEW;
    END IF;
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'lot levels change only through stock entries'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_lot_level_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_lot_level
    FOR EACH ROW EXECUTE FUNCTION stock_lot_level_guard();
CREATE TRIGGER stock_lot_level_no_truncate BEFORE TRUNCATE ON stock_lot_level
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- The lots one entry moved: positive into the warehouse, negative out, as
-- the entry.
CREATE TABLE stock_entry_lot (
    entry_id uuid NOT NULL REFERENCES stock_entry (id),
    lot_id uuid NOT NULL REFERENCES stock_lot (id),
    qty numeric(20, 6) NOT NULL CHECK (qty <> 0),
    PRIMARY KEY (entry_id, lot_id)
);
CREATE INDEX stock_entry_lot_lot ON stock_entry_lot (lot_id);

CREATE FUNCTION stock_entry_lot_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    e stock_entry%ROWTYPE;
    l stock_lot%ROWTYPE;
    itm item%ROWTYPE;
    wh warehouse%ROWTYPE;
    lvl stock_lot_level%ROWTYPE;
    elsewhere numeric;
BEGIN
    SELECT * INTO e FROM stock_entry WHERE id = NEW.entry_id;
    SELECT * INTO l FROM stock_lot WHERE id = NEW.lot_id;
    SELECT * INTO itm FROM item WHERE id = e.item_id;
    IF l.item_id <> e.item_id OR l.company_id <> e.company_id THEN
        RAISE EXCEPTION 'lot % is not a lot of item % in this company', l.code, itm.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF e.qty = 0 OR sign(NEW.qty) <> sign(e.qty) THEN
        RAISE EXCEPTION 'the lots of a stock entry move the way the entry does'
            USING ERRCODE = 'check_violation';
    END IF;
    IF itm.tracking = 'serial' AND abs(NEW.qty) <> 1 THEN
        RAISE EXCEPTION 'serial number % of item % is one unit', l.code, itm.code
            USING ERRCODE = 'check_violation';
    END IF;
    -- A serial number is in one place at a time: its lot row is the lock.
    IF itm.tracking = 'serial' THEN
        PERFORM 1 FROM stock_lot WHERE id = NEW.lot_id FOR UPDATE;
    END IF;
    INSERT INTO stock_lot_level (lot_id, warehouse_id, item_id, company_id)
        VALUES (NEW.lot_id, e.warehouse_id, e.item_id, e.company_id)
        ON CONFLICT DO NOTHING;
    SELECT * INTO lvl FROM stock_lot_level
        WHERE lot_id = NEW.lot_id AND warehouse_id = e.warehouse_id FOR UPDATE;
    IF lvl.qty + NEW.qty < 0 THEN
        SELECT * INTO wh FROM warehouse WHERE id = e.warehouse_id;
        RAISE EXCEPTION 'only % % of lot % of item % are in warehouse %',
            trim_scale(lvl.qty), itm.uom, l.code, itm.code, wh.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF itm.tracking = 'serial' AND NEW.qty > 0 THEN
        SELECT COALESCE(sum(qty), 0) INTO elsewhere FROM stock_lot_level
            WHERE lot_id = NEW.lot_id;
        IF elsewhere + NEW.qty > 1 THEN
            RAISE EXCEPTION 'serial number % of item % is already on hand', l.code, itm.code
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    UPDATE stock_lot_level SET qty = lvl.qty + NEW.qty
        WHERE lot_id = NEW.lot_id AND warehouse_id = e.warehouse_id;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_entry_lot_before_insert BEFORE INSERT ON stock_entry_lot
    FOR EACH ROW EXECUTE FUNCTION stock_entry_lot_before_insert();
CREATE TRIGGER stock_entry_lot_immutable BEFORE UPDATE OR DELETE ON stock_entry_lot
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER stock_entry_lot_no_truncate BEFORE TRUNCATE ON stock_entry_lot
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- At commit: the entry's lots are what its item's tracking asks for.
CREATE FUNCTION stock_entry_lots_complete(entry uuid) RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    e stock_entry%ROWTYPE;
    itm item%ROWTYPE;
    total numeric;
    n integer;
BEGIN
    SELECT * INTO e FROM stock_entry WHERE id = entry;
    -- Shared, so tracking cannot change while the entry commits.
    SELECT * INTO itm FROM item WHERE id = e.item_id FOR SHARE;
    SELECT COALESCE(sum(qty), 0), count(*) INTO total, n FROM stock_entry_lot WHERE entry_id = entry;
    IF e.qty = 0 THEN
        IF n > 0 THEN
            RAISE EXCEPTION 'a cost adjustment moves no lots' USING ERRCODE = 'check_violation';
        END IF;
        RETURN;
    END IF;
    IF e.reverses_id IS NOT NULL THEN
        IF itm.tracking <> 'none' AND NOT EXISTS (
            SELECT 1 FROM stock_entry_lot WHERE entry_id = e.reverses_id) THEN
            RAISE EXCEPTION 'item % has been tracked by % since this movement: it cannot be taken back',
                itm.code, itm.tracking USING ERRCODE = 'check_violation';
        END IF;
        IF EXISTS (
            (SELECT lot_id, -qty FROM stock_entry_lot WHERE entry_id = e.reverses_id
             EXCEPT SELECT lot_id, qty FROM stock_entry_lot WHERE entry_id = entry)
            UNION ALL
            (SELECT lot_id, qty FROM stock_entry_lot WHERE entry_id = entry
             EXCEPT SELECT lot_id, -qty FROM stock_entry_lot WHERE entry_id = e.reverses_id)) THEN
            RAISE EXCEPTION 'a reversal takes back exactly the lots of the entry it reverses'
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN;
    END IF;
    IF itm.tracking = 'none' THEN
        IF n > 0 THEN
            RAISE EXCEPTION 'item % is not tracked by lot: its movements name no lots', itm.code
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF total <> e.qty THEN
        RAISE EXCEPTION 'item % is tracked by %: a movement of % names lots for %',
            itm.code, itm.tracking, trim_scale(abs(e.qty)), trim_scale(abs(total))
            USING ERRCODE = 'check_violation';
    END IF;
END $$;

CREATE FUNCTION stock_entry_lots_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'stock_entry' THEN
        PERFORM stock_entry_lots_complete(NEW.id);
    ELSE
        PERFORM stock_entry_lots_complete(NEW.entry_id);
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER stock_entry_lots_check AFTER INSERT ON stock_entry
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION stock_entry_lots_check();
CREATE CONSTRAINT TRIGGER stock_entry_lot_check AFTER INSERT ON stock_entry_lot
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION stock_entry_lots_check();

-- Tracking changes only while the item has no stock anywhere.
CREATE FUNCTION stock_item_tracking() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tracking IS DISTINCT FROM OLD.tracking AND EXISTS (
        SELECT 1 FROM stock_level WHERE item_id = NEW.id AND (qty <> 0 OR value <> 0)) THEN
        RAISE EXCEPTION 'item % has stock: its tracking changes only when it has none anywhere', NEW.code
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER item_tracking BEFORE UPDATE OF tracking ON item
    FOR EACH ROW EXECUTE FUNCTION stock_item_tracking();

-- The lots a document line names, for every document type that moves
-- stock. Guarded like the document's own lines.
CREATE TABLE stock_line_lot (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_id uuid NOT NULL,
    seq integer NOT NULL,
    lot text NOT NULL CHECK (lot <> '' AND lot = btrim(lot) AND length(lot) <= 64),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- For goods coming in under a new lot.
    expiry_date date,
    manufactured_date date,
    attributes jsonb CHECK (attributes IS NULL OR jsonb_typeof(attributes) = 'object'),
    UNIQUE (document_id, line_id, lot),
    UNIQUE (document_id, line_id, seq)
);
CREATE TRIGGER stock_line_lot_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_line_lot
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
