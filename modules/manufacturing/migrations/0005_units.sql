-- Units of measure in bills of materials and production orders: a bill's
-- output and each of its lines may be in any unit of its item (item_uom),
-- and keep the factor of that unit as it was when they were written. The
-- quantities stay in those units; what is made and consumed is converted
-- into stock units, exactly, when an order is released. A production
-- order's quantity to make is kept in stock units, and shown in its own.
ALTER TABLE bom
    ADD COLUMN output_uom text CHECK (output_uom <> ''),
    ADD COLUMN output_factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (output_factor > 0);
ALTER TABLE bom_line
    ADD COLUMN uom text CHECK (uom <> ''),
    ADD COLUMN factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (factor > 0);
ALTER TABLE production_order
    ADD COLUMN uom text CHECK (uom <> ''),
    ADD COLUMN factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (factor > 0);

-- The factor of a unit of an item: one for its stock unit (or none named),
-- else its item_uom's; refused for a unit the item lacks, or a quantity
-- that is not exact in stock units.
CREATE FUNCTION mfg_unit_factor(the_item uuid, the_uom text, the_qty numeric) RETURNS numeric
LANGUAGE plpgsql STABLE AS $$
DECLARE
    stock text;
    code text;
    f numeric;
BEGIN
    SELECT uom, item.code INTO stock, code FROM item WHERE id = the_item;
    IF the_uom IS NULL OR the_uom = stock THEN
        f := 1;
    ELSE
        SELECT factor INTO f FROM item_uom WHERE item_id = the_item AND uom = the_uom;
        IF f IS NULL THEN
            RAISE EXCEPTION 'item % is kept in % and has no unit %: add it to the item''s units (item_uom) with its factor',
                code, stock, the_uom USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    IF the_qty * f <> round(the_qty * f, 6) THEN
        RAISE EXCEPTION '% % of item % is more places of its stock unit than stock is kept in',
            trim_scale(the_qty), the_uom, code USING ERRCODE = 'check_violation';
    END IF;
    RETURN f;
END $$;

CREATE FUNCTION bom_output_unit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.output_uom = (SELECT uom FROM item WHERE id = NEW.item_id) THEN
        NEW.output_uom := NULL;
    END IF;
    NEW.output_factor := mfg_unit_factor(NEW.item_id, NEW.output_uom, NEW.output_qty);
    RETURN NEW;
END $$;
CREATE TRIGGER bom_output_unit BEFORE INSERT OR UPDATE OF item_id, output_qty, output_uom ON bom
    FOR EACH ROW EXECUTE FUNCTION bom_output_unit();

CREATE FUNCTION bom_line_unit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.uom = (SELECT uom FROM item WHERE id = NEW.item_id) THEN
        NEW.uom := NULL;
    END IF;
    NEW.factor := mfg_unit_factor(NEW.item_id, NEW.uom, NEW.qty);
    RETURN NEW;
END $$;
CREATE TRIGGER bom_line_unit BEFORE INSERT OR UPDATE OF item_id, qty, uom ON bom_line
    FOR EACH ROW EXECUTE FUNCTION bom_line_unit();
