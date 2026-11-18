-- Revaluation of open items in a foreign currency. At a period end an item
-- still open in another currency is worth what is open of it at the rate
-- of that day, not at the rate of its invoice. A revaluation records the
-- difference per item, under a posting that moves the payables or
-- receivables account by it (against unrealized exchange gain or loss),
-- and is reversed the day after, so that the item is carried at its
-- invoice's rate again and a payment realizes its whole difference
-- against that: unrealized and realized differences never count the
-- same movement twice.
--
-- An item's amount and settlements do not change: its value on a day is
-- its amount, less what was settled, plus the revaluations standing that
-- day. That is what the account holds, item by item.
CREATE TABLE open_item_revaluation (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    posting_id uuid NOT NULL REFERENCES posting (id),
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    -- The change in the item's accounting value, in the item's sign: more
    -- than zero when a debt (or a credit) is worth more. Negative on a
    -- reversal, which takes back the revaluation it names.
    amount numeric(20, 4) NOT NULL CHECK (amount <> 0),
    -- The rate of the day, accounting currency per unit of the item's.
    rate numeric(20, 10) NOT NULL CHECK (rate > 0),
    -- What was open of the item in its currency, and what that was carried
    -- at, before the revaluation.
    txn_open numeric(20, 4) NOT NULL,
    carried numeric(20, 4) NOT NULL,
    -- UNIQUE: a revaluation is reversed at most once.
    reverses_id uuid UNIQUE REFERENCES open_item_revaluation (id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX open_item_revaluation_item ON open_item_revaluation (open_item_id);
CREATE INDEX open_item_revaluation_posting ON open_item_revaluation (posting_id);

CREATE FUNCTION open_item_revaluation_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    item open_item%ROWTYPE;
    orig open_item_revaluation%ROWTYPE;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    SELECT * INTO item FROM open_item WHERE id = NEW.open_item_id;
    IF pst.company_id <> NEW.company_id OR item.company_id <> NEW.company_id THEN
        RAISE EXCEPTION 'a revaluation, its posting and its open item must belong to one company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM open_item_revaluation WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL OR orig.open_item_id <> NEW.open_item_id THEN
            RAISE EXCEPTION 'a reversal takes back a revaluation of the same item'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.amount := -orig.amount;
        NEW.rate := orig.rate;
        NEW.txn_open := orig.txn_open;
        NEW.carried := orig.carried + orig.amount;
    ELSIF item.currency = (SELECT base_currency FROM company WHERE id = item.company_id) THEN
        RAISE EXCEPTION 'only an item in a foreign currency is revalued'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER open_item_revaluation_before_insert BEFORE INSERT ON open_item_revaluation
    FOR EACH ROW EXECUTE FUNCTION open_item_revaluation_before_insert();
CREATE TRIGGER open_item_revaluation_immutable BEFORE UPDATE OR DELETE ON open_item_revaluation
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER open_item_revaluation_no_truncate BEFORE TRUNCATE ON open_item_revaluation
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
