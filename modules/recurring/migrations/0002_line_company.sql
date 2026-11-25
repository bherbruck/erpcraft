-- A recurring invoice line is of its template's company, so that the company's grants
-- decide who reads and writes it.
ALTER TABLE recurring_invoice_line ADD COLUMN company_id uuid REFERENCES company (id);
UPDATE recurring_invoice_line l SET company_id = r.company_id FROM recurring_invoice r WHERE r.id = l.recurring_invoice_id;
ALTER TABLE recurring_invoice_line ALTER COLUMN company_id SET NOT NULL;
CREATE INDEX recurring_invoice_line_company ON recurring_invoice_line (company_id);

CREATE FUNCTION recurring_invoice_line_company() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.company_id IS NULL THEN
        NEW.company_id := (SELECT company_id FROM recurring_invoice WHERE id = NEW.recurring_invoice_id);
    ELSIF NEW.company_id IS DISTINCT FROM (SELECT company_id FROM recurring_invoice WHERE id = NEW.recurring_invoice_id) THEN
        RAISE EXCEPTION 'a recurring invoice line is of its template''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER recurring_invoice_line_company BEFORE INSERT OR UPDATE ON recurring_invoice_line
    FOR EACH ROW EXECUTE FUNCTION recurring_invoice_line_company();
