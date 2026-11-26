-- A schedule counts its dates from an anchor: the first invoice, until its
-- frequency or `every` changes; then the date the old schedule would have
-- invoiced next, so that the period the last invoice covered is not
-- invoiced again and no invoice is dated in the past. anchor_count is the
-- number of the invoice on the anchor (from 0).

CREATE FUNCTION recurring_nth(start date, frequency text, every integer, n integer)
RETURNS date LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE frequency
        WHEN 'weekly' THEN start + 7 * every * n
        WHEN 'monthly' THEN (start + make_interval(months => every * n))::date
        WHEN 'quarterly' THEN (start + make_interval(months => 3 * every * n))::date
        WHEN 'yearly' THEN (start + make_interval(years => every * n))::date
    END
$$;

ALTER TABLE recurring_invoice ADD COLUMN anchor_on date;
ALTER TABLE recurring_invoice ADD COLUMN anchor_count integer NOT NULL DEFAULT 0
    CHECK (anchor_count >= 0);
ALTER TABLE recurring_invoice DISABLE TRIGGER recurring_invoice_defaults;
UPDATE recurring_invoice SET anchor_on = starts_on;
ALTER TABLE recurring_invoice ENABLE TRIGGER recurring_invoice_defaults;
ALTER TABLE recurring_invoice ALTER COLUMN anchor_on SET NOT NULL;

CREATE OR REPLACE FUNCTION recurring_invoice_defaults() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.next_date := NEW.starts_on;
        NEW.count := 0;
        NEW.anchor_on := NEW.starts_on;
        NEW.anchor_count := 0;
    ELSIF NEW.starts_on <> OLD.starts_on AND OLD.count > 0 THEN
        RAISE EXCEPTION 'a recurring invoice that has made invoices keeps its start'
            USING ERRCODE = 'check_violation';
    ELSIF NEW.starts_on <> OLD.starts_on THEN
        NEW.next_date := NEW.starts_on;
        NEW.anchor_on := NEW.starts_on;
        NEW.anchor_count := 0;
    ELSIF (NEW.frequency, NEW.every) IS DISTINCT FROM (OLD.frequency, OLD.every)
          AND OLD.count > 0 THEN
        -- The last invoice covers its period by the old schedule: the new
        -- one starts on the date the old one would have invoiced next.
        NEW.anchor_on := OLD.next_date;
        NEW.anchor_count := OLD.count;
        NEW.next_date := OLD.next_date;
    END IF;
    RETURN NEW;
END $$;
