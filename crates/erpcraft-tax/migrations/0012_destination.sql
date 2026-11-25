-- Tax by destination: the code a line is charged chosen by where the goods
-- go, from data. See "Tax by destination" in ARCHITECTURE.md.

-- The jurisdiction a code belongs to: the registration a company must hold
-- to charge it. HST is registered for with the GST, so its region is null.
-- A code without a country is charged by anyone.
ALTER TABLE tax_code
    ADD COLUMN country_code text CHECK (country_code ~ '^[A-Z]{2}$'),
    ADD COLUMN region text CHECK (region <> ''),
    -- What the buyer accounts for when the seller does not charge this code:
    -- a self-assessed code (British Columbia PST on a purchase from outside
    -- the province, US use tax).
    ADD COLUMN self_assessed_code_id uuid REFERENCES tax_code (id),
    -- A line may carry this code only under a customer's exemption
    -- certificate.
    ADD COLUMN exemption_certificate boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT tax_code_region_country CHECK (region IS NULL OR country_code IS NOT NULL),
    ADD CONSTRAINT tax_code_counterpart_other CHECK (self_assessed_code_id <> id);

CREATE FUNCTION tax_code_counterpart_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.self_assessed_code_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM tax_code WHERE id = NEW.self_assessed_code_id AND self_assessed
          AND NOT EXISTS (SELECT 1 FROM tax_code_component g WHERE g.group_id = tax_code.id)
    ) THEN
        RAISE EXCEPTION 'tax code %: what the buyer accounts for instead must be a plain code accounted for by the buyer', NEW.code
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_code_counterpart_check BEFORE INSERT OR UPDATE ON tax_code
    FOR EACH ROW EXECUTE FUNCTION tax_code_counterpart_check();

-- The member states of a union, for the One-Stop Shop. Data, seeded here:
-- the 27 member states of the EU as ISO 3166-1 writes them (Greece is GR,
-- not the EL of VAT numbers).
CREATE TABLE tax_union_member (
    union_code text NOT NULL CHECK (union_code <> ''),
    country_code text NOT NULL CHECK (country_code ~ '^[A-Z]{2}$'),
    PRIMARY KEY (union_code, country_code)
);
INSERT INTO tax_union_member (union_code, country_code)
SELECT 'EU', unnest(ARRAY['AT', 'BE', 'BG', 'CY', 'CZ', 'DE', 'DK', 'EE', 'ES', 'FI', 'FR',
                          'GR', 'HR', 'HU', 'IE', 'IT', 'LT', 'LU', 'LV', 'MT', 'NL', 'PL',
                          'PT', 'RO', 'SE', 'SI', 'SK']);

-- A place and the code charged there. Shared, like tax codes.
CREATE TABLE tax_area (
    id uuid PRIMARY KEY,
    country_code text NOT NULL CHECK (country_code ~ '^[A-Z]{2}$'),
    -- The subdivision as ISO 3166-2 writes it without the country: OH, BC.
    region text CHECK (region <> ''),
    -- Compared on as many characters as postcode_from has, upper case,
    -- without spaces.
    postcode_from text CHECK (postcode_from ~ '^[0-9A-Z-]+$'),
    postcode_to text CHECK (postcode_to ~ '^[0-9A-Z-]+$'),
    city text CHECK (city <> ''),
    -- A group of every jurisdiction there, or one code.
    tax_code_id uuid NOT NULL REFERENCES tax_code (id),
    valid_from date NOT NULL DEFAULT '2000-01-01',
    valid_to date,
    source text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tax_area_postcodes CHECK ((postcode_from IS NULL) = (postcode_to IS NULL)
        AND length(postcode_from) = length(postcode_to) AND postcode_from <= postcode_to),
    CONSTRAINT tax_area_dates CHECK (valid_to IS NULL OR valid_to >= valid_from),
    CONSTRAINT tax_area_once UNIQUE NULLS NOT DISTINCT
        (country_code, region, postcode_from, postcode_to, city, valid_from)
);
CREATE INDEX tax_area_place ON tax_area (country_code, region);

-- Where a company charges tax.
CREATE TABLE tax_registration (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    country_code text NOT NULL CHECK (country_code ~ '^[A-Z]{2}$'),
    -- None: the whole country.
    region text CHECK (region <> ''),
    -- local: registered in that jurisdiction (US nexus in a state, GST/HST,
    -- a provincial tax, VAT in a member state). oss: the EU One-Stop Shop,
    -- registered in the member state of identification (country_code) for
    -- sales to consumers in every other member state.
    scheme text NOT NULL DEFAULT 'local' CHECK (scheme IN ('local', 'oss')),
    number text CHECK (number <> ''),
    valid_from date NOT NULL DEFAULT '2000-01-01',
    valid_to date,
    -- Destination tax only for buyers without a VAT identifier.
    consumers_only boolean NOT NULL DEFAULT false,
    -- oss: distance sales to consumers in a calendar year, in the company's
    -- accounting currency, above which they are taxed at destination.
    threshold numeric(20, 4) CHECK (threshold >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tax_registration_dates CHECK (valid_to IS NULL OR valid_to >= valid_from),
    -- The One-Stop Shop is for consumers whatever consumers_only says.
    CONSTRAINT tax_registration_oss CHECK (scheme = 'local' OR region IS NULL),
    CONSTRAINT tax_registration_threshold CHECK (scheme = 'oss' OR threshold IS NULL),
    CONSTRAINT tax_registration_once UNIQUE NULLS NOT DISTINCT
        (company_id, country_code, region, scheme, valid_from)
);

-- A customer's exemption certificate.
CREATE TABLE tax_exemption (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- Customers are the sales module's, installed after this one: checked
    -- by the trigger below, as tax rules name their scope.
    customer_id uuid NOT NULL,
    country_code text NOT NULL CHECK (country_code ~ '^[A-Z]{2}$'),
    -- None: the whole country.
    region text CHECK (region <> ''),
    certificate_number text NOT NULL CHECK (certificate_number <> ''),
    -- Resale, manufacturing, a charity, a government body.
    reason text NOT NULL CHECK (reason <> ''),
    valid_from date NOT NULL,
    expires_on date,
    -- The code its lines take: one marked exemption_certificate.
    tax_code_id uuid NOT NULL REFERENCES tax_code (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tax_exemption_dates CHECK (expires_on IS NULL OR expires_on >= valid_from),
    CONSTRAINT tax_exemption_once UNIQUE (company_id, customer_id, certificate_number)
);
CREATE INDEX tax_exemption_customer ON tax_exemption (customer_id, company_id);

CREATE FUNCTION tax_exemption_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM customer WHERE id = NEW.customer_id) THEN
        RAISE EXCEPTION 'the customer of an exemption certificate does not exist'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM tax_code WHERE id = NEW.tax_code_id AND exemption_certificate) THEN
        RAISE EXCEPTION 'an exemption certificate gives a tax code marked as one for exemption certificates'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_exemption_check BEFORE INSERT OR UPDATE ON tax_exemption
    FOR EACH ROW EXECUTE FUNCTION tax_exemption_check();

-- What each entry was charged for: the place the goods went, whether the
-- buyer was a consumer (no VAT identifier on the document; null for a
-- purchase or where nothing was kept), and the certificate it was
-- exempted under.
ALTER TABLE tax_entry
    ADD COLUMN country_code text,
    ADD COLUMN region text,
    ADD COLUMN consumer boolean,
    ADD COLUMN exemption_id uuid REFERENCES tax_exemption (id);
CREATE INDEX tax_entry_place ON tax_entry (company_id, country_code, posting_date);

-- A reversal or a credit is for the place and certificate of what it takes
-- back.
CREATE FUNCTION tax_entry_place_copy() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    orig tax_entry%ROWTYPE;
BEGIN
    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM tax_entry WHERE id = NEW.reverses_id;
    ELSIF NEW.credits_id IS NOT NULL THEN
        SELECT * INTO orig FROM tax_entry WHERE id = NEW.credits_id;
    ELSE
        RETURN NEW;
    END IF;
    NEW.country_code := orig.country_code;
    NEW.region := orig.region;
    NEW.consumer := orig.consumer;
    NEW.exemption_id := orig.exemption_id;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_entry_place_copy BEFORE INSERT ON tax_entry
    FOR EACH ROW EXECUTE FUNCTION tax_entry_place_copy();
