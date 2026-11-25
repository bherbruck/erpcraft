-- Units of measure: an item is kept in its stock unit (item.uom) and may
-- be bought and sold in others, each a fixed number of stock units (an
-- exact decimal). Only the stock unit reaches the stock ledger.

CREATE TABLE item_uom (
    id uuid PRIMARY KEY,
    item_id uuid NOT NULL REFERENCES item (id),
    uom text NOT NULL CHECK (uom <> '' AND uom = btrim(uom)),
    -- How many stock units one of this unit is.
    factor numeric(20, 6) NOT NULL CHECK (factor > 0),
    purchase boolean NOT NULL DEFAULT true,
    sales boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (item_id, uom)
);

CREATE FUNCTION item_uom_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM item WHERE id = NEW.item_id AND uom = NEW.uom) THEN
        RAISE EXCEPTION '% is the item''s stock unit already', NEW.uom
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER item_uom_check BEFORE INSERT OR UPDATE ON item_uom
    FOR EACH ROW EXECUTE FUNCTION item_uom_check();
