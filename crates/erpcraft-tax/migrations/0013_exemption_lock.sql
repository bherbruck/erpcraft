-- A certificate that a sale was exempted under says what it said then:
-- once a tax entry names it, only its expiry may change, and not to a day
-- before a sale it exempted. It cannot be deleted (the entries' key).
CREATE FUNCTION tax_exemption_used() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    last_used date;
BEGIN
    SELECT max(posting_date) INTO last_used FROM tax_entry WHERE exemption_id = OLD.id;
    IF last_used IS NULL THEN
        RETURN NEW;
    END IF;
    IF (NEW.company_id, NEW.customer_id, NEW.country_code, NEW.region, NEW.certificate_number,
        NEW.reason, NEW.valid_from, NEW.tax_code_id)
       IS DISTINCT FROM
       (OLD.company_id, OLD.customer_id, OLD.country_code, OLD.region, OLD.certificate_number,
        OLD.reason, OLD.valid_from, OLD.tax_code_id) THEN
        RAISE EXCEPTION 'exemption certificate % has exempted sales: only its expiry may change',
            OLD.certificate_number USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.expires_on IS NOT NULL AND NEW.expires_on < last_used THEN
        RAISE EXCEPTION 'exemption certificate % exempted a sale on %: it cannot expire before',
            OLD.certificate_number, last_used USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_exemption_used BEFORE UPDATE ON tax_exemption
    FOR EACH ROW EXECUTE FUNCTION tax_exemption_used();
