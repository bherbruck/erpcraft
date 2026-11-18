-- Cost adjustments: an entry that changes the value of what is on hand
-- without moving any quantity (qty is zero). A landed cost, such as freight
-- on goods received, raises the value of the goods still on hand this way.
-- The invariants stay: value only where there is stock (never on an empty
-- level), never below zero, and a reversal takes back exactly the entry.
DO $$
DECLARE
    c record;
BEGIN
    FOR c IN SELECT conname FROM pg_constraint
             WHERE conrelid = 'stock_entry'::regclass AND contype = 'c'
               AND (pg_get_constraintdef(oid) LIKE '%qty <> %'
                    OR pg_get_constraintdef(oid) LIKE '%sign(value) = sign(qty)%') LOOP
        EXECUTE format('ALTER TABLE stock_entry DROP CONSTRAINT %I', c.conname);
    END LOOP;
END $$;
ALTER TABLE stock_entry
    ADD CONSTRAINT stock_entry_moves CHECK (qty <> 0 OR value <> 0),
    ADD CONSTRAINT stock_entry_value_way CHECK (qty = 0 OR value = 0 OR sign(value) = sign(qty));

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
    -- Under the lock, so the numbers of one level follow the order its
    -- entries were recorded in.
    NEW.seq := nextval('stock_entry_seq');
    NEW.negative_condition := NULL;

    IF NEW.qty = 0 THEN
        -- A cost adjustment: the value of what is on hand changes, its
        -- quantity does not (a landed cost allocated to goods received).
        -- Only stock that is on hand can change value, and never below
        -- zero; a reversal takes back exactly what was adjusted.
        IF NEW.reverses_id IS NOT NULL THEN
            SELECT * INTO orig FROM stock_entry WHERE id = NEW.reverses_id;
            IF orig.reverses_id IS NOT NULL OR orig.qty <> 0
               OR orig.item_id <> NEW.item_id OR orig.warehouse_id <> NEW.warehouse_id THEN
                RAISE EXCEPTION 'a reversal takes back a cost adjustment of the same item and warehouse'
                    USING ERRCODE = 'check_violation';
            END IF;
            NEW.value := -orig.value;
        ELSIF NEW.value IS NULL OR NEW.value = 0 OR NEW.value <> round(NEW.value, decimals) THEN
            RAISE EXCEPTION 'a cost adjustment changes the value by an amount other than zero, with at most % decimals', decimals
                USING ERRCODE = 'check_violation';
        END IF;
        IF lvl.qty <= 0 THEN
            RAISE EXCEPTION 'none of item % is on hand in warehouse %: there is no stock whose value could change',
                itm.code, wh.code
                USING ERRCODE = 'check_violation';
        END IF;
        IF lvl.value + NEW.value < 0 THEN
            RAISE EXCEPTION 'this would leave item % in warehouse % with a value below zero',
                itm.code, wh.code
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.variance := 0;
        NEW.negative_rule_id := NULL;
        NEW.negative_facts := NULL;
        NEW.unit_cost := 0;
        NEW.qty_after := lvl.qty;
        NEW.value_after := lvl.value + NEW.value;
        UPDATE stock_level SET value = NEW.value_after
            WHERE item_id = NEW.item_id AND warehouse_id = NEW.warehouse_id;
        RETURN NEW;
    END IF;

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
        NEW.negative_facts := NULL;
        -- Taking back the receipt the last cost came from puts back the
        -- cost of the receipt before it. A receipt that a later one has
        -- already superseded changes nothing.
        IF orig.qty > 0 AND NOT EXISTS (
            SELECT 1 FROM stock_entry e
            WHERE e.item_id = orig.item_id AND e.warehouse_id = orig.warehouse_id
              AND e.qty > 0 AND e.reverses_id IS NULL
              AND NOT EXISTS (SELECT 1 FROM stock_entry r WHERE r.reverses_id = e.id)
              AND (COALESCE(e.seq, 0), e.created_at, e.id)
                  > (COALESCE(orig.seq, 0), orig.created_at, orig.id)) THEN
            new_last := stock_last_receipt_cost(orig.item_id, orig.warehouse_id, orig);
        END IF;
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
            NEW.negative_facts := NULL;
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
            -- The facts the rule was asked with must be the facts of this
            -- movement, so that whoever reads the entry can ask the rule's
            -- condition again.
            IF NEW.negative_facts IS NULL OR jsonb_typeof(NEW.negative_facts) <> 'object'
               OR NOT stock_fact_is(NEW.negative_facts, 'on_hand', lvl.qty)
               OR NOT stock_fact_is(NEW.negative_facts, 'issuing', -NEW.qty)
               OR NOT stock_fact_is(NEW.negative_facts, 'short_by', -(lvl.qty + NEW.qty))
               OR NEW.negative_facts ->> 'item_code' IS DISTINCT FROM itm.code
               OR NEW.negative_facts ->> 'warehouse_code' IS DISTINCT FROM wh.code
               OR jsonb_typeof(NEW.negative_facts -> 'context') IS DISTINCT FROM 'string'
               OR NEW.negative_facts ->> 'context' = '' THEN
                RAISE EXCEPTION 'a stock entry that goes below zero must carry the facts its policy rule was asked with, and they must be those of the movement'
                    USING ERRCODE = 'check_violation';
            END IF;
            NEW.negative_condition := COALESCE(rule.condition, 'true');
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
            NEW.negative_facts := NULL;
            -- The last of the stock takes the last of the value with it.
            NEW.value := -lvl.value;
        ELSE
            NEW.negative_rule_id := NULL;
            NEW.negative_facts := NULL;
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
