-- Open items in a foreign currency. An item keeps what is owed in the
-- currency of the invoice (txn_amount, txn_settled) beside what it is
-- worth in the company's accounting currency at the invoice's rate
-- (amount, settled), which is what the control account holds.
--
-- A payment settles an item in the item's currency. What that takes off
-- the item's accounting value is worked out here: its share at the item's
-- own rate, and the whole of what is left when the last of the item is
-- settled, so that a settled item leaves nothing behind in the control
-- account. The difference from what the payment is worth at its own rate
-- is a realized exchange difference, which the payment posts.
--
-- Items and settlements in the accounting currency are what they were:
-- the transaction amount is the accounting amount.

ALTER TABLE open_item DISABLE TRIGGER open_item_guard;

ALTER TABLE open_item
    ADD COLUMN currency text REFERENCES currency (code),
    ADD COLUMN txn_amount numeric(20, 4),
    ADD COLUMN txn_settled numeric(20, 4);
UPDATE open_item i SET currency = c.base_currency, txn_amount = i.amount, txn_settled = i.settled
FROM company c WHERE c.id = i.company_id;
ALTER TABLE open_item
    ALTER COLUMN currency SET NOT NULL,
    ALTER COLUMN txn_amount SET NOT NULL,
    ALTER COLUMN txn_settled SET NOT NULL,
    ALTER COLUMN txn_settled SET DEFAULT 0,
    ADD CONSTRAINT txn_same_sign CHECK (sign(txn_amount) = sign(amount)),
    ADD CONSTRAINT txn_settled_within_amount CHECK (
        (txn_amount > 0 AND txn_settled >= 0 AND txn_settled <= txn_amount)
        OR (txn_amount < 0 AND txn_settled <= 0 AND txn_settled >= txn_amount)),
    -- Settled in its currency means settled in the accounting currency
    -- too: nothing is left behind in the control account.
    ADD CONSTRAINT settled_together CHECK (txn_settled <> txn_amount OR settled = amount);

ALTER TABLE open_item ENABLE TRIGGER open_item_guard;

CREATE OR REPLACE FUNCTION open_item_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
        IF pst.company_id <> NEW.company_id THEN
            RAISE EXCEPTION 'an open item must carry the company of its posting'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NOT EXISTS (SELECT 1 FROM account WHERE id = NEW.account_id AND company_id = NEW.company_id) THEN
            RAISE EXCEPTION 'the account of an open item must belong to its company'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.settled <> 0 OR NEW.txn_settled <> 0 THEN
            RAISE EXCEPTION 'an open item starts unsettled' USING ERRCODE = 'check_violation';
        END IF;
        -- In the accounting currency both amounts are the same.
        IF NEW.currency = (SELECT base_currency FROM company WHERE id = NEW.company_id)
           AND NEW.txn_amount <> NEW.amount THEN
            RAISE EXCEPTION 'an open item in the accounting currency has one amount'
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    -- Only the settlement trigger may change an item, and only how much
    -- of it is settled.
    IF TG_OP = 'UPDATE' AND pg_trigger_depth() >= 2
        AND (to_jsonb(NEW) - 'settled' - 'txn_settled') = (to_jsonb(OLD) - 'settled' - 'txn_settled') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'open items change only through settlements'
        USING ERRCODE = 'check_violation';
END $$;

ALTER TABLE settlement DISABLE TRIGGER settlement_immutable;
ALTER TABLE settlement ADD COLUMN txn_amount numeric(20, 4);
UPDATE settlement SET txn_amount = amount;
-- A small part of an item in a foreign currency can be worth nothing in
-- the accounting currency once rounded: the transaction amount is what
-- cannot be zero.
ALTER TABLE settlement DROP CONSTRAINT settlement_amount_check;
ALTER TABLE settlement
    ALTER COLUMN txn_amount SET NOT NULL,
    ADD CONSTRAINT settlement_txn_amount_check CHECK (txn_amount <> 0),
    ADD CONSTRAINT settlement_txn_same_sign CHECK (sign(txn_amount) = sign(amount) OR amount = 0);
ALTER TABLE settlement ENABLE TRIGGER settlement_immutable;

-- A settlement is given in the item's currency (txn_amount); the trigger
-- works out what it takes off the accounting value (amount). The amount
-- given with the row is ignored for anything but a reversal.
CREATE OR REPLACE FUNCTION settlement_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    item open_item%ROWTYPE;
    orig settlement%ROWTYPE;
    decimals integer;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    -- The row lock makes settlements of one item happen one after the other.
    SELECT * INTO item FROM open_item WHERE id = NEW.open_item_id FOR UPDATE;
    IF pst.company_id <> NEW.company_id OR item.company_id <> NEW.company_id THEN
        RAISE EXCEPTION 'a settlement, its posting and its open item must belong to one company'
            USING ERRCODE = 'check_violation';
    END IF;

    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM settlement WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        IF orig.open_item_id <> NEW.open_item_id THEN
            RAISE EXCEPTION 'a reversal must be for the open item of the settlement it reverses'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.amount := -orig.amount;
        NEW.txn_amount := -orig.txn_amount;
        NEW.cancels := orig.cancels;
    ELSIF NEW.cancels THEN
        IF item.settled <> 0 OR item.txn_settled <> 0 THEN
            RAISE EXCEPTION 'this has been paid in part or in full; reverse the payment first'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.amount := item.amount;
        NEW.txn_amount := item.txn_amount;
    ELSIF NEW.txn_amount IS NULL OR sign(NEW.txn_amount) <> sign(item.txn_amount) THEN
        RAISE EXCEPTION 'a settlement runs the way of its item: more than zero for a debt, less than zero for a credit'
            USING ERRCODE = 'check_violation';
    ELSIF abs(item.txn_settled + NEW.txn_amount) > abs(item.txn_amount) THEN
        RAISE EXCEPTION 'only % is still open, not %',
            trim_scale(item.txn_amount - item.txn_settled), trim_scale(NEW.txn_amount)
            USING ERRCODE = 'check_violation';
    ELSIF item.txn_settled + NEW.txn_amount = item.txn_amount THEN
        -- The last of the item takes the last of its value.
        NEW.amount := item.amount - item.settled;
    ELSE
        SELECT cur.decimals INTO decimals
            FROM company c JOIN currency cur ON cur.code = c.base_currency
            WHERE c.id = item.company_id;
        NEW.amount := round(NEW.txn_amount * item.amount / item.txn_amount, decimals);
        -- Never past what is left of the value.
        IF abs(NEW.amount) > abs(item.amount - item.settled) THEN
            NEW.amount := item.amount - item.settled;
        END IF;
    END IF;

    UPDATE open_item SET settled = settled + NEW.amount, txn_settled = txn_settled + NEW.txn_amount
        WHERE id = NEW.open_item_id;
    RETURN NEW;
END $$;
