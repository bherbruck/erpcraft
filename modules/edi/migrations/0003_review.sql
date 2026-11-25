-- Fixes of the review of phase 1.
--
-- An interchange is now stored in the transaction that reads it, once it
-- is known whose it is: a preview, a caller without the rights, or a
-- failure keeps nothing. Bytes nobody could route are kept per caller, so
-- nobody learns from a repeat what someone else sent.

ALTER TABLE edi_interchange ADD COLUMN received_by uuid REFERENCES app_user (id);
-- An interchange whose control number this partner used before.
ALTER TABLE edi_interchange ADD COLUMN duplicate_of uuid REFERENCES edi_interchange (id);

UPDATE edi_interchange i SET duplicate_of = (
    SELECT f.id FROM edi_interchange f
    WHERE f.partner_id = i.partner_id AND f.direction = 'in' AND f.control = i.control
      AND (f.created_at, f.id) < (i.created_at, i.id)
    ORDER BY f.created_at, f.id LIMIT 1)
WHERE i.direction = 'in' AND i.partner_id IS NOT NULL AND i.control IS NOT NULL;

DROP INDEX edi_interchange_repeat;
CREATE UNIQUE INDEX edi_interchange_repeat ON edi_interchange (sha256)
    WHERE direction = 'in' AND partner_id IS NOT NULL;
CREATE UNIQUE INDEX edi_interchange_repeat_unrouted ON edi_interchange (sha256, received_by)
    NULLS NOT DISTINCT WHERE direction = 'in' AND partner_id IS NULL;
-- A partner's control number names one interchange; a later one with it
-- is kept as its duplicate.
CREATE UNIQUE INDEX edi_interchange_control ON edi_interchange (partner_id, control)
    WHERE direction = 'in' AND partner_id IS NOT NULL AND control IS NOT NULL
      AND duplicate_of IS NULL;

-- Unreadable and unroutable files belong to no company and are evidence
-- of nothing done: edi_interchange.purge deletes old ones. Nothing else
-- is ever deleted.
CREATE OR REPLACE FUNCTION edi_interchange_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF current_setting('edi.purge', true) = 'on' AND OLD.direction = 'in'
           AND OLD.partner_id IS NULL AND OLD.state IN ('unreadable', 'unroutable') THEN
            RETURN OLD;
        END IF;
        RAISE EXCEPTION 'an interchange is never deleted: it is the record of what was exchanged'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.payload IS DISTINCT FROM OLD.payload OR NEW.sha256 IS DISTINCT FROM OLD.sha256
        OR NEW.size IS DISTINCT FROM OLD.size OR NEW.direction IS DISTINCT FROM OLD.direction
        OR (OLD.control IS NOT NULL AND NEW.control IS DISTINCT FROM OLD.control)
        OR (OLD.company_id IS NOT NULL AND NEW.company_id IS DISTINCT FROM OLD.company_id)
        OR (OLD.partner_id IS NOT NULL AND NEW.partner_id IS DISTINCT FROM OLD.partner_id) THEN
        RAISE EXCEPTION 'the bytes of an interchange, their hash, its direction, control number '
                        'and partner never change'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

-- A set whose type is no type is still a message: rejected, and named
-- by nothing.
ALTER TABLE edi_message DROP CONSTRAINT edi_message_message_check;
ALTER TABLE edi_message ADD CONSTRAINT edi_message_message_check
    CHECK (message ~ '^[A-Z0-9]{0,6}$');

-- The n-th sales order drafted for a partner's order number: two drafts
-- of one number at once cannot both be the first.
ALTER TABLE edi_document ADD COLUMN sequence int;
UPDATE edi_document d SET sequence = n.n
FROM (SELECT doc_type, document_id,
             row_number() OVER (PARTITION BY partner_id, order_number
                                ORDER BY created_at, document_id) AS n
      FROM edi_document) n
WHERE n.doc_type = d.doc_type AND n.document_id = d.document_id;
ALTER TABLE edi_document ALTER COLUMN sequence SET NOT NULL;
ALTER TABLE edi_document ADD CONSTRAINT edi_document_sequence CHECK (sequence >= 1);
CREATE UNIQUE INDEX edi_document_number ON edi_document (partner_id, order_number, sequence);

-- Partners route within their company: two companies may trade with the
-- same partner, and an interchange that names both is received with the
-- company it is for.
DROP INDEX edi_partner_route;
CREATE UNIQUE INDEX edi_partner_route ON edi_partner
    (company_id, their_qualifier, their_id, our_qualifier, our_id, usage) WHERE active;

-- Cross-references belong to a company: one company's numbers never
-- resolve another's orders.
ALTER TABLE edi_item_xref ADD COLUMN company_id uuid REFERENCES company (id);
ALTER TABLE edi_uom_xref ADD COLUMN company_id uuid REFERENCES company (id);
ALTER TABLE edi_location_xref ADD COLUMN company_id uuid REFERENCES company (id);
UPDATE edi_item_xref x SET company_id = p.company_id FROM edi_partner p WHERE p.id = x.partner_id;
UPDATE edi_uom_xref x SET company_id = p.company_id FROM edi_partner p WHERE p.id = x.partner_id;
UPDATE edi_location_xref x SET company_id = p.company_id FROM edi_partner p WHERE p.id = x.partner_id;
-- The old unique indexes don't know the company: drop them before a
-- partner-less reference is copied per company, or the copy collides with
-- the row it was copied from.
DROP INDEX edi_item_xref_value;
DROP INDEX edi_uom_xref_code;
-- A reference for every partner was for the companies that had partners.
INSERT INTO edi_item_xref (id, company_id, partner_id, item_id, kind, value, partner_uom,
                           units_per, created_at)
SELECT gen_random_uuid(), c.company_id, NULL, x.item_id, x.kind, x.value, x.partner_uom,
       x.units_per, x.created_at
FROM edi_item_xref x CROSS JOIN (SELECT DISTINCT company_id FROM edi_partner) c
WHERE x.company_id IS NULL;
INSERT INTO edi_uom_xref (id, company_id, partner_id, partner_uom, uom, created_at)
SELECT gen_random_uuid(), c.company_id, NULL, x.partner_uom, x.uom, x.created_at
FROM edi_uom_xref x CROSS JOIN (SELECT DISTINCT company_id FROM edi_partner) c
WHERE x.company_id IS NULL;
DELETE FROM edi_item_xref WHERE company_id IS NULL;
DELETE FROM edi_uom_xref WHERE company_id IS NULL;
ALTER TABLE edi_item_xref ALTER COLUMN company_id SET NOT NULL;
ALTER TABLE edi_uom_xref ALTER COLUMN company_id SET NOT NULL;
ALTER TABLE edi_location_xref ALTER COLUMN company_id SET NOT NULL;

CREATE UNIQUE INDEX edi_item_xref_value ON edi_item_xref (company_id, partner_id, kind, value)
    NULLS NOT DISTINCT;
CREATE UNIQUE INDEX edi_uom_xref_code ON edi_uom_xref (company_id, partner_id, partner_uom)
    NULLS NOT DISTINCT;

CREATE FUNCTION edi_xref_company_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.partner_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM edi_partner p WHERE p.id = NEW.partner_id AND p.company_id = NEW.company_id
    ) THEN
        RAISE EXCEPTION 'the partner must be one of the company''s'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_item_xref_company BEFORE INSERT OR UPDATE ON edi_item_xref
    FOR EACH ROW EXECUTE FUNCTION edi_xref_company_check();
CREATE TRIGGER edi_uom_xref_company BEFORE INSERT OR UPDATE ON edi_uom_xref
    FOR EACH ROW EXECUTE FUNCTION edi_xref_company_check();
CREATE TRIGGER edi_location_xref_company BEFORE INSERT OR UPDATE ON edi_location_xref
    FOR EACH ROW EXECUTE FUNCTION edi_xref_company_check();
