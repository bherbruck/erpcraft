-- The check of a company's IBAN (migration 5) was a NOT VALID constraint,
-- which PostgreSQL checks on every update of the row: an account saved
-- with an IBAN before migration 5 could not be given a routing number
-- without correcting the IBAN first. It is now checked when an IBAN is
-- saved or changed, and an IBAN that was there before is left as it was.
ALTER TABLE bank_account_number DROP CONSTRAINT bank_account_number_iban_valid;

CREATE FUNCTION bank_account_number_iban_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.iban IS NOT NULL
       AND (TG_OP = 'INSERT' OR NEW.iban IS DISTINCT FROM OLD.iban)
       AND NOT banking_iban_valid(NEW.iban) THEN
        RAISE EXCEPTION 'field ''iban'' was refused: % is not a valid IBAN (its check digits do not match)', NEW.iban
            USING ERRCODE = 'check_violation', COLUMN = 'iban';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bank_account_number_iban_check BEFORE INSERT OR UPDATE ON bank_account_number
    FOR EACH ROW EXECUTE FUNCTION bank_account_number_iban_check();
