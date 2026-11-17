-- A document that follows from another: a receipt from a purchase order,
-- an invoice from that order. "from" is the earlier document.

CREATE TABLE document_link (
    from_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    to_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    -- What the later document is to the earlier one, named by the module:
    -- "receipt", "invoice".
    kind text NOT NULL CHECK (kind <> ''),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (from_id, to_id, kind),
    CHECK (from_id <> to_id)
);
CREATE INDEX document_link_to ON document_link (to_id);

CREATE FUNCTION erp_document_link_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT company_id FROM document WHERE id = NEW.from_id)
        IS DISTINCT FROM (SELECT company_id FROM document WHERE id = NEW.to_id) THEN
        RAISE EXCEPTION 'linked documents must belong to the same company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER document_link_check BEFORE INSERT OR UPDATE ON document_link
    FOR EACH ROW EXECUTE FUNCTION erp_document_link_check();
