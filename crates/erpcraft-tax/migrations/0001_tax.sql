-- Tax, as far as every country needs it: a rate, the accounts it posts
-- to, and a record of the tax on each posting to report from. What the
-- rates are, when they apply and how they are reported is for country
-- modules to add on top.

-- Tax codes are shared by all companies.
CREATE TABLE tax_code (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- Percent of the net amount: 19 means 19%.
    rate numeric(9, 6) NOT NULL CHECK (rate >= 0 AND rate <= 100),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Where a company posts a tax code.
CREATE TABLE tax_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    tax_code_id uuid NOT NULL REFERENCES tax_code (id),
    -- Tax charged to customers: owed to the tax authority.
    collected_account_id uuid NOT NULL REFERENCES account (id),
    -- Tax charged by vendors: an asset where it can be reclaimed (VAT), an
    -- expense where it cannot (sales tax).
    paid_account_id uuid NOT NULL REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, tax_code_id)
);

CREATE FUNCTION tax_settings_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM account
        WHERE company_id = NEW.company_id
          AND id IN (NEW.collected_account_id, NEW.paid_account_id))
       <> (SELECT count(DISTINCT a) FROM unnest(ARRAY[NEW.collected_account_id, NEW.paid_account_id]) AS a) THEN
        RAISE EXCEPTION 'the tax accounts must belong to the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_settings_check BEFORE INSERT OR UPDATE ON tax_settings
    FOR EACH ROW EXECUTE FUNCTION tax_settings_check();

-- The tax on one line of one posting. Tax returns are built from these.
CREATE TABLE tax_entry (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    posting_id uuid NOT NULL REFERENCES posting (id),
    line_no integer NOT NULL,
    -- The document line that was taxed, when there is one.
    source_line_id uuid,
    tax_code_id uuid NOT NULL REFERENCES tax_code (id),
    -- collected: charged to a customer. paid: charged by a vendor.
    direction text NOT NULL CHECK (direction IN ('collected', 'paid')),
    posting_date date NOT NULL,
    -- The rate as it was when this was recorded.
    rate numeric(9, 6) NOT NULL,
    -- The net amount taxed and the tax on it, in the company's accounting
    -- currency. Both negative on a reversal.
    base numeric(20, 4) NOT NULL,
    amount numeric(20, 4) NOT NULL,
    -- UNIQUE: an entry can be reversed at most once.
    reverses_id uuid UNIQUE REFERENCES tax_entry (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (posting_id, line_no),
    CHECK (sign(base) * sign(amount) >= 0)
);
CREATE INDEX tax_entry_report ON tax_entry (company_id, posting_date, tax_code_id);

CREATE FUNCTION tax_entry_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    orig tax_entry%ROWTYPE;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    IF pst.company_id <> NEW.company_id OR pst.posting_date <> NEW.posting_date THEN
        RAISE EXCEPTION 'a tax entry must carry the company and date of its posting'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM tax_entry WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.tax_code_id := orig.tax_code_id;
        NEW.direction := orig.direction;
        NEW.rate := orig.rate;
        NEW.base := -orig.base;
        NEW.amount := -orig.amount;
    ELSIF NEW.base < 0 OR NEW.amount < 0 THEN
        RAISE EXCEPTION 'tax is recorded as positive amounts; only a reversal is negative'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_entry_before_insert BEFORE INSERT ON tax_entry
    FOR EACH ROW EXECUTE FUNCTION tax_entry_before_insert();
CREATE TRIGGER tax_entry_immutable BEFORE UPDATE OR DELETE ON tax_entry
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER tax_entry_no_truncate BEFORE TRUNCATE ON tax_entry
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
