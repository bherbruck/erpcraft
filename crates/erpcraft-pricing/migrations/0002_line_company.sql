-- A price list line is of its list's company, so that the company's grants
-- decide who reads and writes it.
ALTER TABLE price_list_line ADD COLUMN company_id uuid REFERENCES company (id);
UPDATE price_list_line l SET company_id = p.company_id FROM price_list p WHERE p.id = l.price_list_id;
ALTER TABLE price_list_line ALTER COLUMN company_id SET NOT NULL;
CREATE INDEX price_list_line_company ON price_list_line (company_id);

CREATE FUNCTION price_list_line_company() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.company_id IS NULL THEN
        NEW.company_id := (SELECT company_id FROM price_list WHERE id = NEW.price_list_id);
    ELSIF NEW.company_id IS DISTINCT FROM (SELECT company_id FROM price_list WHERE id = NEW.price_list_id) THEN
        RAISE EXCEPTION 'a price list line is of its list''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER price_list_line_company BEFORE INSERT OR UPDATE ON price_list_line
    FOR EACH ROW EXECUTE FUNCTION price_list_line_company();

-- A member of a group that does not exist is refused as such, not as an
-- error of the server.
CREATE OR REPLACE FUNCTION price_group_member_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    found boolean;
BEGIN
    NEW.kind := (SELECT kind FROM price_group WHERE id = NEW.group_id);
    IF NEW.kind IS NULL THEN
        RAISE EXCEPTION 'the price group does not exist' USING ERRCODE = 'foreign_key_violation';
    END IF;
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I WHERE id = $1)', NEW.kind)
        INTO found USING NEW.member_id;
    IF NOT found THEN
        RAISE EXCEPTION 'the % of a price group member does not exist', NEW.kind
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
