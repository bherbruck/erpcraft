-- What the company, a customer or a vendor is known by outside the books:
-- its legal particulars, addresses, tax identifiers, electronic addresses
-- and contacts. One schema for all three, keyed by the kind of party and
-- its record, as account rules are keyed by scope; the records themselves
-- stay with the modules that own them. Printing, e-invoices and tax
-- reports read it here.

-- A party is a company, a customer or a vendor that exists.
CREATE FUNCTION party_exists(kind text, id uuid) RETURNS boolean LANGUAGE sql STABLE AS $$
    SELECT CASE kind
        WHEN 'company' THEN EXISTS (SELECT 1 FROM company WHERE company.id = $2)
        WHEN 'customer' THEN EXISTS (SELECT 1 FROM customer WHERE customer.id = $2)
        WHEN 'vendor' THEN EXISTS (SELECT 1 FROM vendor WHERE vendor.id = $2)
        ELSE false
    END
$$;

CREATE FUNCTION party_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT party_exists(NEW.party_type, NEW.party_id) THEN
        RAISE EXCEPTION 'party_id must be a %', NEW.party_type USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.party_type, NEW.party_id) IS DISTINCT FROM (OLD.party_type, OLD.party_id) THEN
        RAISE EXCEPTION 'a party''s record stays with its party' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

-- The legal particulars, one row per party.
CREATE TABLE party_profile (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('company', 'customer', 'vendor')),
    party_id uuid NOT NULL,
    -- The registered name, when it is not the record's name.
    legal_name text CHECK (legal_name <> ''),
    trading_name text CHECK (trading_name <> ''),
    -- The commercial register entry: "HRB 12345, Amtsgericht München".
    legal_registration text CHECK (legal_registration <> ''),
    -- Further legal information: legal form, share capital, directors.
    legal_info text CHECK (legal_info <> ''),
    -- For a customer: the reference it asks its invoices to quote (for a
    -- German public buyer its Leitweg-ID).
    invoice_reference text CHECK (invoice_reference <> ''),
    -- For the company: the bank account it is paid into.
    payment_account_id uuid REFERENCES account (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (party_type, party_id),
    CHECK (payment_account_id IS NULL OR party_type = 'company'),
    CHECK (invoice_reference IS NULL OR party_type = 'customer')
);
CREATE TRIGGER party_profile_check BEFORE INSERT OR UPDATE ON party_profile
    FOR EACH ROW EXECUTE FUNCTION party_check();

-- The company is paid into a bank account of its own.
CREATE FUNCTION party_profile_account_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.payment_account_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM account WHERE id = NEW.payment_account_id
           AND company_id = NEW.party_id AND is_bank) THEN
        RAISE EXCEPTION 'the account paid into must be a bank account of the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER party_profile_account_check BEFORE INSERT OR UPDATE ON party_profile
    FOR EACH ROW EXECUTE FUNCTION party_profile_account_check();

-- Postal addresses: where invoices go (billing) and goods go (shipping),
-- several of each, at most one default of each kind.
CREATE TABLE party_address (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('company', 'customer', 'vendor')),
    party_id uuid NOT NULL,
    kind text NOT NULL DEFAULT 'billing' CHECK (kind IN ('billing', 'shipping')),
    is_default boolean NOT NULL DEFAULT false,
    -- Who or which site at the address, when it is not the party itself.
    attention text CHECK (attention <> ''),
    street text CHECK (street <> ''),
    additional_street text CHECK (additional_street <> ''),
    city text CHECK (city <> ''),
    postcode text CHECK (postcode <> ''),
    -- State, province or region.
    region text CHECK (region <> ''),
    country_code text NOT NULL CHECK (country_code ~ '^[A-Z]{2}$'),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX party_address_of ON party_address (party_type, party_id);
CREATE UNIQUE INDEX party_address_default ON party_address (party_type, party_id, kind)
    WHERE is_default;
CREATE TRIGGER party_address_check BEFORE INSERT OR UPDATE ON party_address
    FOR EACH ROW EXECUTE FUNCTION party_check();

-- Tax identifiers, each with its scheme: an EU VAT identifier, a national
-- tax number (the German Steuernummer), a US EIN, a Canadian business
-- number. One per scheme and party.
CREATE TABLE party_tax_id (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('company', 'customer', 'vendor')),
    party_id uuid NOT NULL,
    scheme text NOT NULL CHECK (scheme IN ('eu_vat', 'tax_number', 'us_ein', 'ca_bn', 'other')),
    value text NOT NULL CHECK (value <> ''),
    -- The country that issued it.
    country_code text CHECK (country_code ~ '^[A-Z]{2}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (party_type, party_id, scheme)
);
-- Incoming invoices find their vendor by VAT identifier.
CREATE INDEX party_tax_id_value ON party_tax_id (scheme, upper(regexp_replace(value, '[^A-Za-z0-9]', '', 'g')));
CREATE TRIGGER party_tax_id_check BEFORE INSERT OR UPDATE ON party_tax_id
    FOR EACH ROW EXECUTE FUNCTION party_check();

-- Where documents reach a party electronically: an email address, a Peppol
-- participant identifier or another scheme of the EAS code list (EM, 0204
-- Leitweg-ID, 9930 German VAT number, 0088 GLN, ...). At most one default.
CREATE TABLE party_electronic_address (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('company', 'customer', 'vendor')),
    party_id uuid NOT NULL,
    scheme text NOT NULL DEFAULT 'EM' CHECK (scheme ~ '^[A-Z0-9]{2,4}$'),
    value text NOT NULL CHECK (value <> ''),
    is_default boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (party_type, party_id, scheme, value)
);
CREATE UNIQUE INDEX party_electronic_address_default
    ON party_electronic_address (party_type, party_id) WHERE is_default;
CREATE TRIGGER party_electronic_address_check BEFORE INSERT OR UPDATE ON party_electronic_address
    FOR EACH ROW EXECUTE FUNCTION party_check();

-- People to talk to, at most one default.
CREATE TABLE party_contact (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('company', 'customer', 'vendor')),
    party_id uuid NOT NULL,
    name text CHECK (name <> ''),
    -- What they do: accounts payable, sales, ...
    role text CHECK (role <> ''),
    phone text CHECK (phone <> ''),
    email text CHECK (email <> ''),
    is_default boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(name, phone, email) > 0)
);
CREATE INDEX party_contact_of ON party_contact (party_type, party_id);
CREATE UNIQUE INDEX party_contact_default ON party_contact (party_type, party_id) WHERE is_default;
CREATE TRIGGER party_contact_check BEFORE INSERT OR UPDATE ON party_contact
    FOR EACH ROW EXECUTE FUNCTION party_check();

-- No orphans: a party's rows go with its record. They never keep a record
-- from being deleted; a record that something else refers to (a posted
-- document) cannot be deleted anyway, and its rows stay with it.
CREATE FUNCTION party_forget() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM party_profile WHERE party_type = TG_ARGV[0] AND party_id = OLD.id;
    DELETE FROM party_address WHERE party_type = TG_ARGV[0] AND party_id = OLD.id;
    DELETE FROM party_tax_id WHERE party_type = TG_ARGV[0] AND party_id = OLD.id;
    DELETE FROM party_electronic_address WHERE party_type = TG_ARGV[0] AND party_id = OLD.id;
    DELETE FROM party_contact WHERE party_type = TG_ARGV[0] AND party_id = OLD.id;
    RETURN OLD;
END $$;
CREATE TRIGGER party_forget AFTER DELETE ON company
    FOR EACH ROW EXECUTE FUNCTION party_forget('company');
CREATE TRIGGER party_forget AFTER DELETE ON customer
    FOR EACH ROW EXECUTE FUNCTION party_forget('customer');
CREATE TRIGGER party_forget AFTER DELETE ON vendor
    FOR EACH ROW EXECUTE FUNCTION party_forget('vendor');
