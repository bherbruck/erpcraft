-- Electronic invoices after EN 16931 (XRechnung, ZUGFeRD / Factur-X):
-- what an invoice must say about the seller and the buyer that the rest of
-- the system does not keep, and which incoming invoices were imported.

-- What a company says about itself on an electronic invoice.
CREATE TABLE einvoice_profile (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    -- BT-27, the company's name when empty.
    legal_name text CHECK (legal_name <> ''),
    -- BT-28.
    trading_name text CHECK (trading_name <> ''),
    -- BT-30: the commercial register entry, such as "HRB 12345, Amtsgericht München".
    legal_registration text CHECK (legal_registration <> ''),
    -- BT-33: further legal information, such as the legal form and capital.
    legal_info text CHECK (legal_info <> ''),
    -- BT-31: the VAT identification number, "DE123456789".
    vat_id text CHECK (vat_id <> ''),
    -- BT-32: the local tax number (Steuernummer).
    tax_number text CHECK (tax_number <> ''),
    street text CHECK (street <> ''),
    additional_street text CHECK (additional_street <> ''),
    city text CHECK (city <> ''),
    postcode text CHECK (postcode <> ''),
    country_code text CHECK (country_code ~ '^[A-Z]{2}$'),
    -- BG-6: who answers questions about an invoice.
    contact_name text CHECK (contact_name <> ''),
    contact_phone text CHECK (contact_phone <> ''),
    contact_email text CHECK (contact_email <> ''),
    -- BT-34: where invoices reach the company, an email address unless
    -- another scheme of the EAS code list is named.
    electronic_address text CHECK (electronic_address <> ''),
    electronic_address_scheme text NOT NULL DEFAULT 'EM' CHECK (electronic_address_scheme ~ '^[A-Z0-9]{2,4}$'),
    -- The bank account customers pay into (BG-17).
    payment_account_id uuid REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The international numbers of a bank account (BT-84, BT-86).
CREATE TABLE einvoice_bank_account (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    account_id uuid NOT NULL UNIQUE REFERENCES account (id),
    iban text NOT NULL CHECK (iban ~ '^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$'),
    bic text CHECK (bic ~ '^[A-Z0-9]{8}([A-Z0-9]{3})?$'),
    -- BT-85, the account holder's name when it differs from the company's.
    account_name text CHECK (account_name <> ''),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A bank account and a payment account are the company's own.
CREATE FUNCTION einvoice_bank_check(acc uuid, company uuid) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF acc IS NOT NULL AND NOT EXISTS (SELECT 1 FROM account WHERE id = acc
                                       AND company_id = company AND is_bank) THEN
        RAISE EXCEPTION 'the account must be a bank account of the same company'
            USING ERRCODE = 'check_violation';
    END IF;
END $$;
CREATE FUNCTION einvoice_profile_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM einvoice_bank_check(NEW.payment_account_id, NEW.company_id);
    RETURN NEW;
END $$;
CREATE FUNCTION einvoice_bank_account_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM einvoice_bank_check(NEW.account_id, NEW.company_id);
    RETURN NEW;
END $$;
CREATE TRIGGER einvoice_profile_check BEFORE INSERT OR UPDATE ON einvoice_profile
    FOR EACH ROW EXECUTE FUNCTION einvoice_profile_check();
CREATE TRIGGER einvoice_bank_account_check BEFORE INSERT OR UPDATE ON einvoice_bank_account
    FOR EACH ROW EXECUTE FUNCTION einvoice_bank_account_check();

-- What an invoice says about a customer (as buyer) or a vendor (as seller)
-- that the customer and vendor records do not keep. Shared, like them.
CREATE TABLE einvoice_party (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('customer', 'vendor')),
    party_id uuid NOT NULL,
    -- The party's name when empty.
    legal_name text CHECK (legal_name <> ''),
    vat_id text CHECK (vat_id <> ''),
    tax_number text CHECK (tax_number <> ''),
    legal_registration text CHECK (legal_registration <> ''),
    street text CHECK (street <> ''),
    additional_street text CHECK (additional_street <> ''),
    city text CHECK (city <> ''),
    postcode text CHECK (postcode <> ''),
    country_code text CHECK (country_code ~ '^[A-Z]{2}$'),
    -- BT-10: the reference the buyer asks to be quoted; for a German
    -- public buyer its Leitweg-ID.
    buyer_reference text CHECK (buyer_reference <> ''),
    electronic_address text CHECK (electronic_address <> ''),
    electronic_address_scheme text NOT NULL DEFAULT 'EM' CHECK (electronic_address_scheme ~ '^[A-Z0-9]{2,4}$'),
    contact_name text CHECK (contact_name <> ''),
    contact_phone text CHECK (contact_phone <> ''),
    contact_email text CHECK (contact_email <> ''),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (party_type, party_id)
);
-- Incoming invoices find their vendor by VAT identifier.
CREATE INDEX einvoice_party_vat ON einvoice_party (party_type, upper(regexp_replace(vat_id, '[^A-Za-z0-9]', '', 'g')));

CREATE FUNCTION einvoice_party_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.party_type = 'customer' AND NOT EXISTS (SELECT 1 FROM customer WHERE id = NEW.party_id))
       OR (NEW.party_type = 'vendor' AND NOT EXISTS (SELECT 1 FROM vendor WHERE id = NEW.party_id)) THEN
        RAISE EXCEPTION 'party_id must be a % ', NEW.party_type USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER einvoice_party_check BEFORE INSERT OR UPDATE ON einvoice_party
    FOR EACH ROW EXECUTE FUNCTION einvoice_party_check();

-- The VAT category of EN 16931 (UNTDID 5305 as the standard restricts it)
-- a tax code stands for, with the reason for an exemption. Without a row a
-- code with a rate above zero is standard rated (S); one at zero must say.
CREATE TABLE einvoice_tax_category (
    id uuid PRIMARY KEY,
    tax_code_id uuid NOT NULL UNIQUE REFERENCES tax_code (id),
    category text NOT NULL CHECK (category IN ('S', 'Z', 'E', 'AE', 'K', 'G', 'O', 'L', 'M')),
    -- BT-120 and BT-121: required for E, AE, K, G and O.
    exemption_reason text CHECK (exemption_reason <> ''),
    exemption_reason_code text CHECK (exemption_reason_code <> ''),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Incoming electronic invoices taken in: one vendor invoice number of a
-- vendor is imported once per company.
CREATE TABLE einvoice_import (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    vendor_id uuid NOT NULL REFERENCES vendor (id),
    invoice_number text NOT NULL,
    issue_date date NOT NULL,
    syntax text NOT NULL CHECK (syntax IN ('ubl', 'cii')),
    -- The SHA-256 of the XML as received, in hexadecimal.
    sha256 text NOT NULL,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, vendor_id, invoice_number)
);
