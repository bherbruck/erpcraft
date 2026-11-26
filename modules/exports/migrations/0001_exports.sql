-- What a company's outside accountant gets: DATEV files, information
-- returns (1099, T5018) and the general ledger. See "Accountant exports"
-- in ARCHITECTURE.md.

-- The DATEV particulars of a company.
CREATE TABLE datev_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    consultant_number integer NOT NULL CHECK (consultant_number BETWEEN 1001 AND 9999999),
    client_number integer NOT NULL CHECK (client_number BETWEEN 1 AND 99999),
    account_length integer NOT NULL DEFAULT 4 CHECK (account_length BETWEEN 4 AND 8),
    chart text NOT NULL DEFAULT '03' CHECK (chart ~ '^[0-9]{2}$'),
    fiscal_year_start_month integer NOT NULL DEFAULT 1
        CHECK (fiscal_year_start_month BETWEEN 1 AND 12),
    dictation_code text CHECK (dictation_code ~ '^[A-Za-z0-9]{1,2}$'),
    kost1_dimension_id uuid REFERENCES dimension (id),
    kost2_dimension_id uuid REFERENCES dimension (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The BU-Schlüssel of a tax code, on sales and on purchases.
CREATE TABLE datev_tax_key (
    id uuid PRIMARY KEY,
    tax_code_id uuid NOT NULL UNIQUE REFERENCES tax_code (id),
    sales_key text CHECK (sales_key ~ '^[0-9]{1,4}$'),
    purchase_key text CHECK (purchase_key ~ '^[0-9]{1,4}$'),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- An account DATEV knows as an automatic account (Automatikkonto): it
-- works the tax out itself, and a line on it carries no key.
CREATE TABLE datev_account_setting (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    account_id uuid NOT NULL UNIQUE REFERENCES account (id),
    automatic boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION datev_account_setting_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM account WHERE id = NEW.account_id AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'the account must be one of the company''s'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER datev_account_setting_check BEFORE INSERT OR UPDATE ON datev_account_setting
    FOR EACH ROW EXECUTE FUNCTION datev_account_setting_check();

-- A customer's or vendor's personal account (Debitor, Kreditor) in DATEV.
CREATE TABLE datev_party_account (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    party_type text NOT NULL CHECK (party_type IN ('customer', 'vendor')),
    party_id uuid NOT NULL,
    account_number integer NOT NULL CHECK (account_number BETWEEN 10000 AND 999999999),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, party_type, party_id),
    UNIQUE (company_id, account_number)
);

-- How a vendor is reported to a tax authority: on a 1099 in a box, or on
-- a T5018.
CREATE TABLE vendor_information_return (
    id uuid PRIMARY KEY,
    vendor_id uuid NOT NULL UNIQUE REFERENCES vendor (id),
    form text NOT NULL CHECK (form IN ('us_1099', 'ca_t5018')),
    box text CHECK (box IN ('nec_1', 'misc_1', 'misc_2', 'misc_3', 'misc_6', 'misc_10')),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((form = 'us_1099') = (box IS NOT NULL))
);

-- Every file that left: what, for which range, by whom, and its hash.
-- Never changed, never deleted.
CREATE TABLE export_log (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    kind text NOT NULL CHECK (kind <> ''),
    from_date date,
    to_date date,
    rows integer NOT NULL CHECK (rows >= 0),
    filename text NOT NULL,
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    fixed boolean NOT NULL DEFAULT false,
    created_by uuid NOT NULL REFERENCES app_user (id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX export_log_company ON export_log (company_id, created_at);
CREATE TRIGGER export_log_immutable BEFORE UPDATE OR DELETE ON export_log
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER export_log_no_truncate BEFORE TRUNCATE ON export_log
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
