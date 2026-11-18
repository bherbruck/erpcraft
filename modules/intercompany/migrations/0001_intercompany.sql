-- Trading between companies of one database.
--
-- A customer or vendor (shared by all companies) can stand for one of the
-- companies. A rule per selling company and customer says that what is
-- sold to that customer is mirrored as a draft in the buying company: an
-- approved sales order as a purchase order, a posted sales invoice as a
-- vendor invoice, both of the vendor that stands for the seller, made as
-- a user of the buyer.

CREATE TABLE intercompany_party (
    id uuid PRIMARY KEY,
    party_type text NOT NULL CHECK (party_type IN ('customer', 'vendor')),
    party_id uuid NOT NULL,
    company_id uuid NOT NULL REFERENCES company (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    -- A party stands for at most one company.
    UNIQUE (party_type, party_id)
);

CREATE FUNCTION intercompany_party_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (NEW.party_type = 'customer' AND NOT EXISTS (SELECT 1 FROM customer WHERE id = NEW.party_id))
        OR (NEW.party_type = 'vendor' AND NOT EXISTS (SELECT 1 FROM vendor WHERE id = NEW.party_id)) THEN
        RAISE EXCEPTION 'the % does not exist', NEW.party_type USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER intercompany_party_check BEFORE INSERT OR UPDATE ON intercompany_party
    FOR EACH ROW EXECUTE FUNCTION intercompany_party_check();

CREATE TABLE intercompany_rule (
    id uuid PRIMARY KEY,
    -- The selling company.
    company_id uuid NOT NULL REFERENCES company (id),
    buyer_company_id uuid NOT NULL REFERENCES company (id),
    customer_id uuid NOT NULL REFERENCES customer (id),
    vendor_id uuid NOT NULL REFERENCES vendor (id),
    run_as_user_id uuid NOT NULL REFERENCES app_user (id),
    warehouse_id uuid REFERENCES warehouse (id),
    account_id uuid REFERENCES account (id),
    mirror_orders boolean NOT NULL DEFAULT true,
    mirror_invoices boolean NOT NULL DEFAULT true,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (company_id <> buyer_company_id),
    UNIQUE (company_id, customer_id)
);

CREATE FUNCTION intercompany_rule_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM intercompany_party WHERE party_type = 'customer'
                   AND party_id = NEW.customer_id AND company_id = NEW.buyer_company_id) THEN
        RAISE EXCEPTION 'the customer of an intercompany rule must stand for the buying company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM intercompany_party WHERE party_type = 'vendor'
                   AND party_id = NEW.vendor_id AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'the vendor of an intercompany rule must stand for the selling company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.warehouse_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM warehouse WHERE id = NEW.warehouse_id AND company_id = NEW.buyer_company_id) THEN
        RAISE EXCEPTION 'the warehouse of an intercompany rule must be the buying company''s'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.account_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM account WHERE id = NEW.account_id AND company_id = NEW.buyer_company_id) THEN
        RAISE EXCEPTION 'the account of an intercompany rule must be the buying company''s'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER intercompany_rule_check BEFORE INSERT OR UPDATE ON intercompany_rule
    FOR EACH ROW EXECUTE FUNCTION intercompany_rule_check();

-- What became of each document that was to be mirrored: its mirror, or
-- why there is none. One row per source document.
CREATE TABLE intercompany_document (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id uuid NOT NULL UNIQUE REFERENCES document (id) ON DELETE CASCADE,
    company_id uuid NOT NULL REFERENCES company (id),
    buyer_company_id uuid NOT NULL REFERENCES company (id),
    rule_id uuid NOT NULL REFERENCES intercompany_rule (id),
    -- Empty once the buyer deleted the draft, or when it was refused.
    mirror_id uuid REFERENCES document (id) ON DELETE SET NULL,
    status text NOT NULL CHECK (status IN ('drafted', 'refused')),
    message text,
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX intercompany_document_mirror ON intercompany_document (mirror_id);

-- Which line of a mirror stands for which line of its source.
CREATE TABLE intercompany_line (
    source_line_id uuid PRIMARY KEY,
    mirror_line_id uuid NOT NULL,
    source_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    mirror_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE
);
