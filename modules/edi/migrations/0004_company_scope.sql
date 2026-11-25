-- Review 8.
--
-- The same bytes received for two companies are two interchanges: a
-- repeat is a repeat within one company, so nobody is handed another
-- company's interchange for sending the same file.
DROP INDEX edi_interchange_repeat;
CREATE UNIQUE INDEX edi_interchange_repeat ON edi_interchange (company_id, sha256)
    WHERE direction = 'in' AND partner_id IS NOT NULL;

-- A location cross-reference never points at an address that is gone or
-- that is not its partner's customer's. When the address is removed, or
-- the partner is given another customer, the cross-reference stays (its
-- store number is still known) without an address, and an order to it
-- asks for one, as for a place nobody named.
ALTER TABLE edi_location_xref ALTER COLUMN address_id DROP NOT NULL;
ALTER TABLE edi_location_xref DISABLE TRIGGER edi_location_xref_check;
UPDATE edi_location_xref x SET address_id = NULL
WHERE NOT EXISTS (SELECT 1 FROM edi_partner p
                  JOIN party_address a ON a.party_type = 'customer' AND a.party_id = p.customer_id
                  WHERE p.id = x.partner_id AND a.id = x.address_id);
ALTER TABLE edi_location_xref ENABLE TRIGGER edi_location_xref_check;
ALTER TABLE edi_location_xref ADD CONSTRAINT edi_location_xref_address
    FOREIGN KEY (address_id) REFERENCES party_address (id) ON DELETE SET NULL;

CREATE OR REPLACE FUNCTION edi_location_xref_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.address_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM edi_partner p
        JOIN party_address a ON a.party_type = 'customer' AND a.party_id = p.customer_id
        WHERE p.id = NEW.partner_id AND a.id = NEW.address_id) THEN
        RAISE EXCEPTION 'the address must be one of the addresses of the partner''s customer'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.warehouse_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM warehouse w JOIN edi_partner p ON p.company_id = w.company_id
        WHERE w.id = NEW.warehouse_id AND p.id = NEW.partner_id
    ) THEN
        RAISE EXCEPTION 'the warehouse must belong to the partner''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

-- A partner given another customer: its places keep their numbers and
-- lose the old customer's addresses.
CREATE FUNCTION edi_partner_customer_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE edi_location_xref x SET address_id = NULL
    WHERE x.partner_id = NEW.id AND x.address_id IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM party_address a
                      WHERE a.id = x.address_id AND a.party_type = 'customer'
                        AND a.party_id = NEW.customer_id);
    RETURN NEW;
END $$;
CREATE TRIGGER edi_partner_customer_changed AFTER UPDATE OF customer_id ON edi_partner
    FOR EACH ROW WHEN (OLD.customer_id IS DISTINCT FROM NEW.customer_id)
    EXECUTE FUNCTION edi_partner_customer_changed();
