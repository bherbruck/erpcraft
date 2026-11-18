-- What of a settlement was an early-payment discount: the part of
-- txn_amount (in the item's currency) the party kept back rather than
-- paid. A credit note that later credits the invoice takes the discount
-- back in proportion, so that crediting all of it leaves nothing on the
-- discount accounts and the tax ledger. A reversal takes the discount back
-- with the settlement.
ALTER TABLE settlement DISABLE TRIGGER settlement_immutable;
ALTER TABLE settlement
    ADD COLUMN discount numeric(20, 4) NOT NULL DEFAULT 0,
    ADD CONSTRAINT settlement_discount_within
        CHECK (discount = 0 OR (sign(discount) = sign(txn_amount) AND abs(discount) <= abs(txn_amount)));
ALTER TABLE settlement ENABLE TRIGGER settlement_immutable;

CREATE FUNCTION settlement_discount_reversal() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.reverses_id IS NOT NULL THEN
        NEW.discount := -(SELECT discount FROM settlement WHERE id = NEW.reverses_id);
    ELSIF NEW.cancels THEN
        NEW.discount := 0;
    END IF;
    RETURN NEW;
END $$;
-- Named to run after settlement_before_insert, which sets the amounts.
CREATE TRIGGER settlement_z_discount BEFORE INSERT ON settlement
    FOR EACH ROW EXECUTE FUNCTION settlement_discount_reversal();
