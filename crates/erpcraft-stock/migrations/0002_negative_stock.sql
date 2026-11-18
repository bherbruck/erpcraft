-- Stock may go below zero where the company's policy allows it (the policy
-- point stock.negative). The default refuses it, as before.
--
-- A level below zero is goods that left before the system knew they had
-- arrived. They have no real cost yet, so they leave at an estimate: the
-- level's last known unit cost. The level then carries a negative value,
-- the estimate of what is missing, so that the value of the stock still
-- equals the inventory account.
--
-- When goods arrive on a level below zero they first cover what is
-- missing. For those units the difference between what they really cost
-- and what the shortfall was carried at is a variance: it is not stock
-- value. The entry records it, and the caller posts it to the general
-- ledger. The rest of the goods come into stock at their real cost.

ALTER TABLE stock_level
    -- The unit cost of the most recent receipt: what goods that are not
    -- on hand are estimated at. NULL: nothing was ever received here.
    ADD COLUMN last_cost numeric(20, 6) CHECK (last_cost >= 0);

DO $$
DECLARE
    c record;
BEGIN
    FOR c IN SELECT conname FROM pg_constraint
             WHERE conrelid = 'stock_level'::regclass AND contype = 'c'
               AND conname <> 'stock_level_last_cost_check' LOOP
        EXECUTE format('ALTER TABLE stock_level DROP CONSTRAINT %I', c.conname);
    END LOOP;
END $$;

-- Quantity and value always point the same way, and an empty shelf is
-- worth nothing.
ALTER TABLE stock_level ADD CONSTRAINT stock_level_value_follows_qty CHECK (
    (qty > 0 AND value >= 0) OR (qty = 0 AND value = 0) OR (qty < 0 AND value <= 0)
);

ALTER TABLE stock_level DISABLE TRIGGER stock_level_guard;
UPDATE stock_level l SET last_cost = COALESCE(
    (SELECT e.unit_cost FROM stock_entry e
     WHERE e.item_id = l.item_id AND e.warehouse_id = l.warehouse_id
       AND e.qty > 0 AND e.reverses_id IS NULL
     ORDER BY e.created_at DESC, e.id DESC LIMIT 1),
    CASE WHEN l.qty > 0 THEN round(l.value / l.qty, 6) END);
ALTER TABLE stock_level ENABLE TRIGGER stock_level_guard;

CREATE OR REPLACE FUNCTION stock_level_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' AND NEW.qty = 0 AND NEW.value = 0 AND NEW.last_cost IS NULL THEN
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

ALTER TABLE stock_entry
    -- Set by the ledger on a receipt that covers a level below zero: what
    -- the covering goods really cost, less what the shortfall was carried
    -- at. Positive: they cost more than estimated. value + variance is
    -- what the receipt cost in all.
    ADD COLUMN variance numeric(20, 4) NOT NULL DEFAULT 0,
    -- Set on an issue that took the level below zero: the policy rule that
    -- allowed it. The ledger honours it only if it is an active rule of
    -- the point stock.negative that allows, for this company.
    ADD COLUMN negative_rule_id uuid REFERENCES policy_rule (id);

CREATE OR REPLACE FUNCTION stock_entry_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    wh warehouse%ROWTYPE;
    itm item%ROWTYPE;
    lvl stock_level%ROWTYPE;
    orig stock_entry%ROWTYPE;
    rule policy_rule%ROWTYPE;
    decimals integer;
    new_qty numeric;
    new_value numeric;
    new_last numeric;
    cost numeric;
    short numeric;
    cost_cover numeric;
    carried numeric;
    estimate numeric;
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
    new_last := lvl.last_cost;

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
        NEW.variance := -orig.variance;
        NEW.negative_rule_id := NULL;
    ELSE
        IF NOT wh.active THEN
            RAISE EXCEPTION 'warehouse % is inactive', wh.code USING ERRCODE = 'check_violation';
        END IF;
        IF NOT itm.active THEN
            RAISE EXCEPTION 'item % is inactive', itm.code USING ERRCODE = 'check_violation';
        END IF;
        NEW.variance := 0;
        IF NEW.qty > 0 THEN
            -- A receipt comes in at the cost the caller states.
            IF NEW.value IS NULL OR NEW.value < 0 OR NEW.value <> round(NEW.value, decimals) THEN
                RAISE EXCEPTION 'a receipt needs a value of zero or more, with at most % decimals', decimals
                    USING ERRCODE = 'check_violation';
            END IF;
            NEW.negative_rule_id := NULL;
            cost := NEW.value;
            new_last := round(cost / NEW.qty, 6);
            IF lvl.qty < 0 THEN
                -- The goods first cover what is missing. What they cost
                -- beyond what the shortfall was carried at is a variance,
                -- not stock value.
                short := -lvl.qty;
                IF NEW.qty > short THEN
                    cost_cover := round(cost * short / NEW.qty, decimals);
                    carried := -lvl.value;
                ELSIF NEW.qty = short THEN
                    cost_cover := cost;
                    carried := -lvl.value;
                ELSE
                    cost_cover := cost;
                    carried := LEAST(-lvl.value, round(-lvl.value * NEW.qty / short, decimals));
                END IF;
                NEW.variance := cost_cover - carried;
                NEW.value := cost - NEW.variance;
            END IF;
        ELSIF lvl.qty + NEW.qty < 0 THEN
            -- More than is on hand: only where a policy rule allows it.
            IF NEW.negative_rule_id IS NULL THEN
                RAISE EXCEPTION 'only % % of item % are on hand in warehouse %',
                    trim_scale(lvl.qty), itm.uom, itm.code, wh.code
                    USING ERRCODE = 'check_violation';
            END IF;
            SELECT * INTO rule FROM policy_rule WHERE id = NEW.negative_rule_id;
            IF NOT FOUND OR rule.point <> 'stock.negative' OR rule.effect <> 'allow' OR NOT rule.active
               OR (rule.company_id IS NOT NULL AND rule.company_id <> NEW.company_id) THEN
                RAISE EXCEPTION 'the policy rule named on this stock entry does not allow stock to go below zero in this company'
                    USING ERRCODE = 'check_violation';
            END IF;
            -- What is on hand leaves with all of its value; what is not
            -- leaves at the last known cost, or at nothing if none is known.
            estimate := COALESCE(lvl.last_cost, 0);
            IF lvl.qty > 0 THEN
                NEW.value := -(lvl.value + round((-NEW.qty - lvl.qty) * estimate, decimals));
            ELSE
                NEW.value := -round(-NEW.qty * estimate, decimals);
            END IF;
        ELSIF lvl.qty + NEW.qty = 0 THEN
            NEW.negative_rule_id := NULL;
            -- The last of the stock takes the last of the value with it.
            NEW.value := -lvl.value;
        ELSE
            NEW.negative_rule_id := NULL;
            -- An issue goes out at the average cost of what is on hand.
            NEW.value := -LEAST(lvl.value, round(-NEW.qty * lvl.value / lvl.qty, decimals));
        END IF;
    END IF;

    new_qty := lvl.qty + NEW.qty;
    new_value := lvl.value + NEW.value;
    -- Taking a receipt back may leave the level below zero only as far as
    -- it stood below zero before that receipt.
    IF NEW.reverses_id IS NOT NULL AND orig.qty > 0
       AND new_qty < LEAST(orig.qty_after - orig.qty, 0) THEN
        RAISE EXCEPTION 'this would leave % % of item % in warehouse %; the stock has already moved on',
            trim_scale(new_qty), itm.uom, itm.code, wh.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF (new_qty > 0 AND new_value < 0) OR (new_qty = 0 AND new_value <> 0)
       OR (new_qty < 0 AND new_value > 0) THEN
        RAISE EXCEPTION 'this would leave item % in warehouse % with a quantity of % and a value of %; the stock received has since been issued at another cost',
            itm.code, wh.code, trim_scale(new_qty), new_value
            USING ERRCODE = 'check_violation';
    END IF;

    NEW.unit_cost := round(NEW.value / NEW.qty, 6);
    NEW.qty_after := new_qty;
    NEW.value_after := new_value;
    UPDATE stock_level SET qty = new_qty, value = new_value, last_cost = new_last
        WHERE item_id = NEW.item_id AND warehouse_id = NEW.warehouse_id;
    RETURN NEW;
END $$;
