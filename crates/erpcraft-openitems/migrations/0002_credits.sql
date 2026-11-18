-- Open items of the other sign: a credit note, or money paid or received
-- on account, is an item the party is owed back against its debts. Its
-- amount is below zero, and so is what has been settled of it. A payment
-- settles debts and credits of one party together; a payment of nothing
-- (a credit applied to an invoice) settles both and moves no money.
--
-- The rules stay as they were, for either sign: an item is never settled
-- beyond its amount, a settlement runs the way of its item, and open items
-- add up to the accounts that carry them.

ALTER TABLE open_item DROP CONSTRAINT open_item_amount_check;
ALTER TABLE open_item ADD CONSTRAINT open_item_amount_check CHECK (amount <> 0);
ALTER TABLE open_item DROP CONSTRAINT settled_within_amount;
ALTER TABLE open_item ADD CONSTRAINT settled_within_amount CHECK (
    (amount > 0 AND settled >= 0 AND settled <= amount)
    OR (amount < 0 AND settled <= 0 AND settled >= amount)
);

CREATE OR REPLACE FUNCTION settlement_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    item open_item%ROWTYPE;
    orig settlement%ROWTYPE;
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
        NEW.cancels := orig.cancels;
    ELSIF NEW.cancels THEN
        IF item.settled <> 0 THEN
            RAISE EXCEPTION 'this has been paid in part or in full; reverse the payment first'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.amount := item.amount;
    ELSIF sign(NEW.amount) <> sign(item.amount) THEN
        RAISE EXCEPTION 'a settlement runs the way of its item: more than zero for a debt, less than zero for a credit'
            USING ERRCODE = 'check_violation';
    ELSIF abs(item.settled + NEW.amount) > abs(item.amount) THEN
        RAISE EXCEPTION 'only % is still open, not %',
            trim_scale(item.amount - item.settled), trim_scale(NEW.amount)
            USING ERRCODE = 'check_violation';
    END IF;

    UPDATE open_item SET settled = settled + NEW.amount WHERE id = NEW.open_item_id;
    RETURN NEW;
END $$;
