-- The unit an item is bought in from the vendor a reorder rule names: one
-- of the item's purchase units (item_uom). Planning drafts purchase orders
-- in it, rounded up to whole units of it; left out, in the stock unit.
ALTER TABLE reorder_rule ADD COLUMN purchase_uom text CHECK (purchase_uom <> '');

CREATE FUNCTION reorder_rule_unit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    stock text;
    code text;
BEGIN
    IF NEW.purchase_uom IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT uom, item.code INTO stock, code FROM item WHERE id = NEW.item_id;
    IF NEW.purchase_uom = stock THEN
        NEW.purchase_uom := NULL;
    ELSIF NOT EXISTS (SELECT 1 FROM item_uom WHERE item_id = NEW.item_id
                      AND uom = NEW.purchase_uom AND purchase) THEN
        RAISE EXCEPTION 'item % is not bought in %: name one of its purchase units (item_uom)',
            code, NEW.purchase_uom USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER reorder_rule_unit BEFORE INSERT OR UPDATE OF item_id, purchase_uom ON reorder_rule
    FOR EACH ROW EXECUTE FUNCTION reorder_rule_unit();
