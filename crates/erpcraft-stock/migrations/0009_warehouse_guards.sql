-- What a warehouse is cannot change under the stock it holds.
--
-- Its value is carried on the account and with the dimension values its
-- place gives it: the account for goods in transit in a transit
-- warehouse, the site's values at its site and those of the warehouses it
-- is part of. Changing the transit flag, the site or the parent of a
-- warehouse that holds stock (or of one whose locations hold stock) would
-- leave that value where it was posted while the stock ledger says it is
-- elsewhere. Move the stock out first.
--
-- The tree is also changed one at a time per company: the check that a
-- location is not part of itself reads the tree, and two changes at once
-- could each pass and together make a loop.

CREATE OR REPLACE FUNCTION stock_warehouse_tree() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    parent warehouse%ROWTYPE;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('warehouse_tree:' || NEW.company_id::text, 0));
    IF TG_OP = 'UPDATE'
        AND (NEW.transit IS DISTINCT FROM OLD.transit
             OR NEW.site_id IS DISTINCT FROM OLD.site_id
             OR NEW.parent_id IS DISTINCT FROM OLD.parent_id)
        -- A location's site follows its parent's, set by the parent's
        -- change, which was checked for the whole tree below it.
        AND pg_trigger_depth() < 2
        AND EXISTS (
            WITH RECURSIVE down AS (
                SELECT NEW.id AS id
                UNION
                SELECT w.id FROM warehouse w JOIN down ON w.parent_id = down.id
            )
            SELECT 1 FROM stock_level l JOIN down ON l.warehouse_id = down.id
            WHERE l.qty <> 0 OR l.value <> 0
        ) THEN
        RAISE EXCEPTION 'warehouse % holds stock: its transit flag, site and place cannot change until the stock is moved out', NEW.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.parent_id IS NOT NULL THEN
        SELECT * INTO parent FROM warehouse WHERE id = NEW.parent_id;
        IF parent.company_id <> NEW.company_id THEN
            RAISE EXCEPTION 'a location must be part of a warehouse of the same company'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.site_id IS NULL THEN
            NEW.site_id := parent.site_id;
        ELSIF NEW.site_id IS DISTINCT FROM parent.site_id THEN
            RAISE EXCEPTION 'a location is at the site of the warehouse it is part of'
                USING ERRCODE = 'check_violation';
        END IF;
        IF TG_OP = 'UPDATE' AND EXISTS (
            WITH RECURSIVE up AS (
                SELECT id, parent_id FROM warehouse WHERE id = NEW.parent_id
                UNION
                SELECT w.id, w.parent_id FROM warehouse w JOIN up ON w.id = up.parent_id
            )
            SELECT 1 FROM up WHERE id = NEW.id
        ) THEN
            RAISE EXCEPTION 'a location cannot be part of itself'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END $$;
