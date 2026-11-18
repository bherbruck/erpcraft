-- A credit note takes back tax that an invoice charged. It is recorded as
-- an entry of its own, dated in the credit note's period, so that a period
-- already reported does not change: negative base and amount, at the code,
-- direction and rate of the invoice's entry it names (credits_id). What is
-- credited of an entry never exceeds the entry: neither its base nor its
-- amount.

ALTER TABLE tax_entry ADD COLUMN credits_id uuid REFERENCES tax_entry (id);
CREATE INDEX tax_entry_credits ON tax_entry (credits_id) WHERE credits_id IS NOT NULL;

CREATE OR REPLACE FUNCTION tax_entry_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    orig tax_entry%ROWTYPE;
    credited tax_entry%ROWTYPE;
    taken_base numeric;
    taken_amount numeric;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    IF pst.company_id <> NEW.company_id OR pst.posting_date <> NEW.posting_date THEN
        RAISE EXCEPTION 'a tax entry must carry the company and date of its posting'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM tax_entry WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.tax_code_id := orig.tax_code_id;
        NEW.direction := orig.direction;
        NEW.rate := orig.rate;
        NEW.base := -orig.base;
        NEW.amount := -orig.amount;
        -- Reversing a credit gives back what it had taken.
        NEW.credits_id := orig.credits_id;
    ELSIF NEW.credits_id IS NOT NULL THEN
        -- The entry credited, locked so that two credits of it take turns.
        SELECT * INTO credited FROM tax_entry WHERE id = NEW.credits_id FOR UPDATE;
        IF credited.company_id <> NEW.company_id OR credited.reverses_id IS NOT NULL
           OR credited.credits_id IS NOT NULL OR credited.base <= 0 THEN
            RAISE EXCEPTION 'a credit takes back tax that an invoice recorded, in the same company'
                USING ERRCODE = 'check_violation';
        END IF;
        IF EXISTS (SELECT 1 FROM tax_entry WHERE reverses_id = credited.id) THEN
            RAISE EXCEPTION 'the tax this credit would take back was reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.base >= 0 OR NEW.amount > 0 THEN
            RAISE EXCEPTION 'a credit is recorded as negative amounts'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.tax_code_id := credited.tax_code_id;
        NEW.direction := credited.direction;
        NEW.rate := credited.rate;
        SELECT COALESCE(sum(base), 0), COALESCE(sum(amount), 0) INTO taken_base, taken_amount
            FROM tax_entry WHERE credits_id = credited.id;
        IF credited.base + taken_base + NEW.base < 0
           OR credited.amount + taken_amount + NEW.amount < 0 THEN
            RAISE EXCEPTION 'more tax would be credited than the invoice charged'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.base < 0 OR NEW.amount < 0 THEN
        RAISE EXCEPTION 'tax is recorded as positive amounts; only a reversal or a credit is negative'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
