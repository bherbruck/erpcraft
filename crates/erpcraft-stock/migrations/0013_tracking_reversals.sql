-- Review 8: an item's tracking and the reversals that cross it.
--
-- A movement of lots taken back after its item stopped being tracked
-- brought its lot levels back for an untracked item; the item, tracked
-- again later, could then go below zero. Such a reversal is refused, as
-- the reversal of an untracked movement of a since tracked item already
-- was. And tracking changes only while the item has no stock and no lot
-- of it holds anything, anywhere.

CREATE OR REPLACE FUNCTION stock_entry_lots_complete(entry uuid) RETURNS void LANGUAGE plpgsql AS $$
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
        -- And the other way round: lots brought back for an item that is
        -- no longer tracked would stand beside its untracked stock, and
        -- once it is tracked again the item could go below zero.
        IF itm.tracking = 'none' AND EXISTS (
            SELECT 1 FROM stock_entry_lot WHERE entry_id = e.reverses_id) THEN
            RAISE EXCEPTION 'item % is no longer tracked by lot since this movement: it cannot be taken back',
                itm.code USING ERRCODE = 'check_violation';
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

CREATE OR REPLACE FUNCTION stock_item_tracking() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tracking IS DISTINCT FROM OLD.tracking AND (
        EXISTS (SELECT 1 FROM stock_level WHERE item_id = NEW.id AND (qty <> 0 OR value <> 0))
        OR EXISTS (SELECT 1 FROM stock_lot_level WHERE item_id = NEW.id AND qty <> 0)) THEN
        RAISE EXCEPTION 'item % has stock: its tracking changes only when it has none anywhere', NEW.code
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
