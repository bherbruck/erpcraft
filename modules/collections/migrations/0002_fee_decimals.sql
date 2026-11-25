-- A dunning level's fee is charged as it is: at most the decimals of the
-- company's accounting currency.
CREATE FUNCTION dunning_level_fee_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    places integer;
BEGIN
    SELECT c.decimals INTO places FROM company co JOIN currency c ON c.code = co.base_currency
        WHERE co.id = NEW.company_id;
    IF NEW.fee <> round(NEW.fee, places) THEN
        RAISE EXCEPTION 'a dunning fee has at most % decimal places', places
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER dunning_level_fee_check BEFORE INSERT OR UPDATE ON dunning_level
    FOR EACH ROW EXECUTE FUNCTION dunning_level_fee_check();
