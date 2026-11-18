-- A reversal is what it reverses: the reversal of an adjustment (tax taken
-- back by a credit not tied to an invoice) is an adjustment too, so a
-- return or report that picks out adjustments sees the credit and its
-- reversal net to nothing. Migration 9 copied everything else from the
-- original but not this flag. Entries already reversed are set right.
CREATE FUNCTION tax_entry_adjustment_reversal() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.reverses_id IS NOT NULL THEN
        NEW.adjustment := (SELECT adjustment FROM tax_entry WHERE id = NEW.reverses_id);
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_entry_adjustment_reversal BEFORE INSERT ON tax_entry
    FOR EACH ROW EXECUTE FUNCTION tax_entry_adjustment_reversal();

-- Tax entries cannot be changed (tax_entry_immutable); this one correction
-- of a flag the reversal should have carried goes around it.
ALTER TABLE tax_entry DISABLE TRIGGER tax_entry_immutable;
UPDATE tax_entry r SET adjustment = true
FROM tax_entry o
WHERE r.reverses_id = o.id AND o.adjustment AND NOT r.adjustment;
ALTER TABLE tax_entry ENABLE TRIGGER tax_entry_immutable;
