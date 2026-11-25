-- Recurring invoices: a customer invoiced on a schedule. See "Recurring
-- invoices" in ARCHITECTURE.md.

CREATE TABLE recurring_invoice (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    customer_id uuid NOT NULL REFERENCES customer (id),
    -- None: the accounting currency.
    currency text REFERENCES currency (code),
    payment_terms_id uuid REFERENCES payment_terms (id),
    frequency text NOT NULL CHECK (frequency IN ('weekly', 'monthly', 'quarterly', 'yearly')),
    -- Every so many of the frequency: 2 with monthly is every two months.
    every integer NOT NULL DEFAULT 1 CHECK (every BETWEEN 1 AND 52),
    -- The date of the first invoice; the others are counted from it, so a
    -- monthly template on the 31st falls on the last day of shorter months.
    starts_on date NOT NULL,
    -- Kept by the run: the date of the next invoice.
    next_date date NOT NULL,
    end_date date,
    max_count integer CHECK (max_count > 0),
    -- Kept by the run: invoices made.
    count integer NOT NULL DEFAULT 0 CHECK (count >= 0),
    -- Post what it makes, as far as the policy recurring_invoice.post allows.
    post boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CHECK (end_date IS NULL OR end_date >= starts_on)
);

CREATE FUNCTION recurring_invoice_defaults() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.next_date := NEW.starts_on;
        NEW.count := 0;
    ELSIF NEW.starts_on <> OLD.starts_on AND OLD.count > 0 THEN
        RAISE EXCEPTION 'a recurring invoice that has made invoices keeps its start'
            USING ERRCODE = 'check_violation';
    ELSIF NEW.starts_on <> OLD.starts_on THEN
        NEW.next_date := NEW.starts_on;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER recurring_invoice_defaults BEFORE INSERT OR UPDATE ON recurring_invoice
    FOR EACH ROW EXECUTE FUNCTION recurring_invoice_defaults();

CREATE TABLE recurring_invoice_line (
    id uuid PRIMARY KEY,
    recurring_invoice_id uuid NOT NULL REFERENCES recurring_invoice (id) ON DELETE CASCADE,
    line_no integer NOT NULL DEFAULT 1 CHECK (line_no > 0),
    charge_id uuid REFERENCES charge (id),
    account_id uuid REFERENCES account (id),
    description text,
    qty numeric(20, 6) NOT NULL DEFAULT 1 CHECK (qty > 0),
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    -- None: what the tax rules propose.
    tax_code_id uuid REFERENCES tax_code (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(charge_id, account_id) = 1)
);
CREATE INDEX recurring_invoice_line_template ON recurring_invoice_line (recurring_invoice_id);

-- Each invoice a template made, by its date: once only.
CREATE TABLE recurring_invoice_run (
    recurring_invoice_id uuid NOT NULL REFERENCES recurring_invoice (id),
    invoice_date date NOT NULL,
    invoice_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    posted boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (recurring_invoice_id, invoice_date)
);
