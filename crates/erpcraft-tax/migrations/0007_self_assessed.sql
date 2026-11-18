-- Tax the buyer accounts for itself: EU reverse charge and intra-Community
-- acquisitions, US use tax. On a purchase the vendor charges none; the
-- company owes the tax and, where it may, reclaims it: the paid account is
-- debited and the collected account credited by the same amount, and both
-- are recorded, so the return shows the tax due and the tax reclaimed.
-- On a sale the seller charges none and records the base alone.
ALTER TABLE tax_code ADD COLUMN self_assessed boolean NOT NULL DEFAULT false;

-- An entry the company owes or reclaims itself rather than one a party was
-- charged: not part of what the invoice is owed for.
ALTER TABLE tax_entry ADD COLUMN self_assessed boolean NOT NULL DEFAULT false;

-- Reversals and credits are what they answer.
CREATE FUNCTION tax_entry_self_assessed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.reverses_id IS NOT NULL THEN
        NEW.self_assessed := (SELECT self_assessed FROM tax_entry WHERE id = NEW.reverses_id);
    ELSIF NEW.credits_id IS NOT NULL THEN
        NEW.self_assessed := (SELECT self_assessed FROM tax_entry WHERE id = NEW.credits_id);
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_entry_self_assessed BEFORE INSERT ON tax_entry
    FOR EACH ROW EXECUTE FUNCTION tax_entry_self_assessed();
