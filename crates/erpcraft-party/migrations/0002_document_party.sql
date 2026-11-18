-- What a document showed of its parties when it was posted: the seller,
-- the buyer and where the goods went, copied from the party's data. A
-- posted invoice prints and is sent with the address it was posted with,
-- even after the customer moves. Written by the posting command before
-- the document is locked; the document child guard keeps it unchanged
-- from then on.
CREATE TABLE document_party (
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('seller', 'buyer', 'ship_to')),
    party_type text NOT NULL CHECK (party_type IN ('company', 'customer', 'vendor')),
    party_id uuid NOT NULL,
    -- The party's address it was copied from, while it was on record.
    address_id uuid,
    name text,
    legal_name text,
    trading_name text,
    legal_registration text,
    legal_info text,
    invoice_reference text,
    payment_account_id uuid,
    attention text,
    street text,
    additional_street text,
    city text,
    postcode text,
    region text,
    -- Null when the party had no address of the kind.
    country_code text CHECK (country_code ~ '^[A-Z]{2}$'),
    -- [[scheme, value], ...]
    tax_ids jsonb NOT NULL DEFAULT '[]',
    electronic_scheme text,
    electronic_address text,
    contact_name text,
    contact_phone text,
    contact_email text,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (document_id, role)
);
CREATE INDEX document_party_party ON document_party (party_type, party_id);
CREATE TRIGGER document_party_check BEFORE INSERT OR UPDATE ON document_party
    FOR EACH ROW EXECUTE FUNCTION party_check();
CREATE TRIGGER document_party_guard BEFORE INSERT OR UPDATE OR DELETE ON document_party
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
