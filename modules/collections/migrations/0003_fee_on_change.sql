-- Review 9. Migration 2 checked a dunning level's fee on every update, so
-- a level saved before it with a fee of more places than its currency has
-- (numeric(20, 4) took 2.505) could no longer be renamed or changed at
-- all. As banking 6 did for IBANs: the fee is checked when it is written,
-- on a new level or when the fee or the company changes, and a level kept
-- from before is left as it is until then.
CREATE OR REPLACE FUNCTION dunning_level_fee_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    places integer;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.fee = OLD.fee AND NEW.company_id = OLD.company_id THEN
        RETURN NEW;
    END IF;
    SELECT c.decimals INTO places FROM company co JOIN currency c ON c.code = co.base_currency
        WHERE co.id = NEW.company_id;
    IF NEW.fee <> round(NEW.fee, places) THEN
        RAISE EXCEPTION 'a dunning fee has at most % decimal places', places
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
