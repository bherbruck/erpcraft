-- A tax entry names the general ledger account its tax was posted to, so
-- that a credit takes the tax back from that account even when the company
-- has since named another one for the code.
--
-- Entries written before this did not keep it, and the general ledger
-- line cannot be told apart from others of the same amount on the same
-- document line. They take the account the company's settings name for
-- their code and direction now, which is the account they were posted to
-- unless the settings were changed since.
ALTER TABLE tax_entry ADD COLUMN account_id uuid REFERENCES account (id);

ALTER TABLE tax_entry DISABLE TRIGGER tax_entry_immutable;
UPDATE tax_entry e
SET account_id = CASE e.direction WHEN 'collected' THEN s.collected_account_id
                                  ELSE s.paid_account_id END
FROM tax_settings s
WHERE s.company_id = e.company_id AND s.tax_code_id = e.tax_code_id;
ALTER TABLE tax_entry ENABLE TRIGGER tax_entry_immutable;

-- Reversals and credits take the account of the entry they answer.
CREATE FUNCTION tax_entry_account() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.reverses_id IS NOT NULL THEN
        NEW.account_id := (SELECT account_id FROM tax_entry WHERE id = NEW.reverses_id);
    ELSIF NEW.credits_id IS NOT NULL THEN
        NEW.account_id := (SELECT account_id FROM tax_entry WHERE id = NEW.credits_id);
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_entry_account BEFORE INSERT ON tax_entry
    FOR EACH ROW EXECUTE FUNCTION tax_entry_account();
