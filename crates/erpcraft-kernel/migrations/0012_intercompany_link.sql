-- Linked documents belong to one company, except a document and its
-- mirror in another company (kind 'intercompany'): a sales invoice and the
-- vendor invoice it was drafted into. A reader sees the other side of a
-- link only if they may read that document.
CREATE OR REPLACE FUNCTION erp_document_link_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.kind <> 'intercompany'
        AND (SELECT company_id FROM document WHERE id = NEW.from_id)
            IS DISTINCT FROM (SELECT company_id FROM document WHERE id = NEW.to_id) THEN
        RAISE EXCEPTION 'linked documents must belong to the same company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
