-- A budget line is of its budget's company, so that the company's grants
-- decide who reads and writes it.
ALTER TABLE budget_line DISABLE TRIGGER budget_line_check;
ALTER TABLE budget_line ADD COLUMN company_id uuid REFERENCES company (id);
UPDATE budget_line l SET company_id = b.company_id FROM budget b WHERE b.id = l.budget_id;
ALTER TABLE budget_line ALTER COLUMN company_id SET NOT NULL;
ALTER TABLE budget_line ENABLE TRIGGER budget_line_check;
CREATE INDEX budget_line_company ON budget_line (company_id);

CREATE FUNCTION budget_line_company() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.company_id IS NULL THEN
        NEW.company_id := (SELECT company_id FROM budget WHERE id = NEW.budget_id);
    ELSIF NEW.company_id IS DISTINCT FROM (SELECT company_id FROM budget WHERE id = NEW.budget_id) THEN
        RAISE EXCEPTION 'a budget line is of its budget''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER budget_line_company BEFORE INSERT OR UPDATE ON budget_line
    FOR EACH ROW EXECUTE FUNCTION budget_line_company();
