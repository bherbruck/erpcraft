-- Stock ledger. As with the general ledger, the rules here are enforced by
-- the database, so they hold for every module and for raw SQL.
--
-- Valuation is moving average per item and warehouse: a receipt comes in
-- at its own cost, an issue goes out at the average cost of what is on
-- hand at that moment. Entries are valued when they are recorded; a
-- back-dated entry does not revalue the entries recorded after it.

-- Items are shared by all companies.
CREATE TABLE item (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- Unit of measure in which quantities are kept.
    uom text NOT NULL DEFAULT 'each' CHECK (uom <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE warehouse (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code)
);

-- What is on hand: the running total of stock_entry, kept by its trigger.
-- Its row is also the lock that makes movements of one item in one
-- warehouse happen one after the other.
CREATE TABLE stock_level (
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    company_id uuid NOT NULL REFERENCES company (id),
    qty numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty >= 0),
    -- In the company's accounting currency.
    value numeric(20, 4) NOT NULL DEFAULT 0 CHECK (value >= 0),
    PRIMARY KEY (item_id, warehouse_id),
    CHECK (qty > 0 OR value = 0)
);

-- Only the stock_entry trigger may change a level. Anyone may create an
-- empty one, which is how a caller takes the lock in a fixed order.
CREATE FUNCTION stock_level_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.qty = 0 AND NEW.value = 0 THEN
        RETURN NEW;
    END IF;
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'stock levels change only through stock entries'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_level_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_level
    FOR EACH ROW EXECUTE FUNCTION stock_level_guard();
CREATE TRIGGER stock_level_no_truncate BEFORE TRUNCATE ON stock_level
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- One movement of one item in one warehouse.
CREATE TABLE stock_entry (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    posting_id uuid NOT NULL REFERENCES posting (id),
    line_no integer NOT NULL,
    -- The document line that caused this entry, when there is one.
    source_line_id uuid,
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    posting_date date NOT NULL,
    -- Positive: into the warehouse. Negative: out of it.
    qty numeric(20, 6) NOT NULL CHECK (qty <> 0),
    -- The change in stock value, in the company's accounting currency.
    -- Given by the caller for a receipt; set by the ledger for an issue and
    -- for a reversal.
    value numeric(20, 4) NOT NULL,
    -- value / qty, for reading. The value is what counts.
    unit_cost numeric(20, 6) NOT NULL,
    -- The level after this entry.
    qty_after numeric(20, 6) NOT NULL,
    value_after numeric(20, 4) NOT NULL,
    -- UNIQUE: an entry can be reversed at most once.
    reverses_id uuid UNIQUE REFERENCES stock_entry (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (posting_id, line_no),
    CHECK (value = 0 OR sign(value) = sign(qty))
);
CREATE INDEX stock_entry_item ON stock_entry (item_id, warehouse_id, created_at);
CREATE INDEX stock_entry_company_date ON stock_entry (company_id, posting_date);

CREATE FUNCTION stock_entry_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    wh warehouse%ROWTYPE;
    itm item%ROWTYPE;
    lvl stock_level%ROWTYPE;
    orig stock_entry%ROWTYPE;
    decimals integer;
    new_qty numeric;
    new_value numeric;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    IF pst.company_id <> NEW.company_id OR pst.posting_date <> NEW.posting_date THEN
        RAISE EXCEPTION 'a stock entry must carry the company and date of its posting'
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT * INTO wh FROM warehouse WHERE id = NEW.warehouse_id;
    IF wh.company_id <> NEW.company_id THEN
        RAISE EXCEPTION 'warehouse % belongs to another company', wh.code
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT * INTO itm FROM item WHERE id = NEW.item_id;
    SELECT cur.decimals INTO decimals
        FROM company c JOIN currency cur ON cur.code = c.base_currency
        WHERE c.id = NEW.company_id;

    -- Take the level's row lock. Movements of this item in this warehouse
    -- now wait for this transaction.
    INSERT INTO stock_level (item_id, warehouse_id, company_id)
        VALUES (NEW.item_id, NEW.warehouse_id, NEW.company_id)
        ON CONFLICT DO NOTHING;
    SELECT * INTO lvl FROM stock_level
        WHERE item_id = NEW.item_id AND warehouse_id = NEW.warehouse_id
        FOR UPDATE;

    IF NEW.reverses_id IS NOT NULL THEN
        -- A reversal takes back exactly what the original recorded.
        SELECT * INTO orig FROM stock_entry WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        IF orig.item_id <> NEW.item_id OR orig.warehouse_id <> NEW.warehouse_id THEN
            RAISE EXCEPTION 'a reversal must be for the item and warehouse of the entry it reverses'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.qty := -orig.qty;
        NEW.value := -orig.value;
    ELSE
        IF NOT wh.active THEN
            RAISE EXCEPTION 'warehouse % is inactive', wh.code USING ERRCODE = 'check_violation';
        END IF;
        IF NOT itm.active THEN
            RAISE EXCEPTION 'item % is inactive', itm.code USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.qty > 0 THEN
            -- A receipt comes in at the cost the caller states.
            IF NEW.value IS NULL OR NEW.value < 0 OR NEW.value <> round(NEW.value, decimals) THEN
                RAISE EXCEPTION 'a receipt needs a value of zero or more, with at most % decimals', decimals
                    USING ERRCODE = 'check_violation';
            END IF;
        ELSIF lvl.qty + NEW.qty < 0 THEN
            RAISE EXCEPTION 'only % % of item % are on hand in warehouse %',
                trim_scale(lvl.qty), itm.uom, itm.code, wh.code
                USING ERRCODE = 'check_violation';
        ELSIF lvl.qty + NEW.qty = 0 THEN
            -- The last of the stock takes the last of the value with it.
            NEW.value := -lvl.value;
        ELSE
            -- An issue goes out at the average cost of what is on hand.
            NEW.value := -LEAST(lvl.value, round(-NEW.qty * lvl.value / lvl.qty, decimals));
        END IF;
    END IF;

    new_qty := lvl.qty + NEW.qty;
    new_value := lvl.value + NEW.value;
    IF new_qty < 0 THEN
        RAISE EXCEPTION 'this would leave % % of item % in warehouse %; the stock has already moved on',
            trim_scale(new_qty), itm.uom, itm.code, wh.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF new_value < 0 OR (new_qty = 0 AND new_value <> 0) THEN
        RAISE EXCEPTION 'this would leave item % in warehouse % with a quantity of % and a value of %; the stock received has since been issued at another cost',
            itm.code, wh.code, trim_scale(new_qty), new_value
            USING ERRCODE = 'check_violation';
    END IF;

    NEW.unit_cost := round(NEW.value / NEW.qty, 6);
    NEW.qty_after := new_qty;
    NEW.value_after := new_value;
    UPDATE stock_level SET qty = new_qty, value = new_value
        WHERE item_id = NEW.item_id AND warehouse_id = NEW.warehouse_id;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_entry_before_insert BEFORE INSERT ON stock_entry
    FOR EACH ROW EXECUTE FUNCTION stock_entry_before_insert();

CREATE TRIGGER stock_entry_immutable BEFORE UPDATE OR DELETE ON stock_entry
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER stock_entry_no_truncate BEFORE TRUNCATE ON stock_entry
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
