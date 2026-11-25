-- What a payment run needs of bank accounts, kept where bank numbers are
-- kept: here.
--
-- The company's own accounts get what US banks need: the ABA routing
-- number, and the company identification and immediate origin a NACHA file
-- carries. Vendors' (and customers') accounts get a table of their own.

CREATE FUNCTION banking_iban_valid(iban text) RETURNS boolean LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    moved text := substr(iban, 5) || substr(iban, 1, 4);
    digits text := '';
    c text;
    remainder integer := 0;
    i integer;
BEGIN
    IF iban !~ '^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$' THEN
        RETURN false;
    END IF;
    FOR i IN 1 .. length(moved) LOOP
        c := substr(moved, i, 1);
        IF c ~ '[A-Z]' THEN
            digits := digits || (ascii(c) - 55)::text;
        ELSE
            digits := digits || c;
        END IF;
    END LOOP;
    FOR i IN 1 .. length(digits) LOOP
        remainder := (remainder * 10 + substr(digits, i, 1)::integer) % 97;
    END LOOP;
    RETURN remainder = 1;
END $$;

CREATE FUNCTION banking_routing_valid(routing text) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
    SELECT routing ~ '^[0-9]{9}$' AND (
        3 * (substr(routing, 1, 1)::int + substr(routing, 4, 1)::int + substr(routing, 7, 1)::int)
      + 7 * (substr(routing, 2, 1)::int + substr(routing, 5, 1)::int + substr(routing, 8, 1)::int)
      + (substr(routing, 3, 1)::int + substr(routing, 6, 1)::int + substr(routing, 9, 1)::int)
    ) % 10 = 0
$$;

ALTER TABLE bank_account_number
    ADD COLUMN routing_number text CHECK (banking_routing_valid(routing_number)),
    ADD COLUMN ach_company_id text CHECK (ach_company_id ~ '^[ -~]{1,10}$'),
    ADD COLUMN ach_origin text CHECK (ach_origin ~ '^[ -~]{1,10}$'),
    ADD CONSTRAINT bank_account_number_iban_valid CHECK (iban IS NULL OR banking_iban_valid(iban)) NOT VALID;

CREATE TABLE party_bank_account (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('vendor', 'customer')),
    party_id uuid NOT NULL,
    -- Without spaces, in capitals.
    iban text CHECK (iban IS NULL OR banking_iban_valid(iban)),
    bic text CHECK (bic ~ '^[A-Z0-9]{8}([A-Z0-9]{3})?$'),
    routing_number text CHECK (banking_routing_valid(routing_number)),
    account_number text CHECK (account_number ~ '^[0-9A-Za-z-]{1,17}$'),
    account_type text NOT NULL DEFAULT 'checking' CHECK (account_type IN ('checking', 'savings')),
    -- The name the account is held in, when it is not the party's.
    account_name text CHECK (account_name <> ''),
    currency text REFERENCES currency (code),
    is_default boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- An IBAN, or an account number (with a routing number for a US bank).
    CHECK (iban IS NOT NULL OR account_number IS NOT NULL)
);
-- One default per party.
CREATE UNIQUE INDEX party_bank_account_default ON party_bank_account (party_type, party_id)
    WHERE is_default;
CREATE INDEX party_bank_account_party ON party_bank_account (party_type, party_id);

CREATE FUNCTION party_bank_account_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.party_type = 'vendor' AND NOT EXISTS (SELECT 1 FROM vendor WHERE id = NEW.party_id))
        OR (NEW.party_type = 'customer' AND NOT EXISTS (SELECT 1 FROM customer WHERE id = NEW.party_id)) THEN
        RAISE EXCEPTION 'the % does not exist', NEW.party_type USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER party_bank_account_check BEFORE INSERT OR UPDATE ON party_bank_account
    FOR EACH ROW EXECUTE FUNCTION party_bank_account_check();
