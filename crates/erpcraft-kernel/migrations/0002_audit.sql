-- Audit: record commands that failed, and which records each command
-- touched, so that the history of one record can be read.

ALTER TABLE audit_log
    ADD COLUMN outcome text NOT NULL DEFAULT 'ok' CHECK (outcome IN ('ok', 'error')),
    -- Set when outcome is 'error': the error's code and what the caller was told.
    ADD COLUMN error_code text,
    ADD COLUMN error_message text;
CREATE INDEX audit_log_user ON audit_log (user_id, at);

-- A record a command created, changed or acted on. "resource" is an entity
-- name, a document type, or "user".
CREATE TABLE audit_subject (
    audit_id uuid NOT NULL REFERENCES audit_log (id),
    resource text NOT NULL,
    record_id uuid NOT NULL,
    PRIMARY KEY (audit_id, resource, record_id)
);
-- audit_log ids are time-ordered, so this index returns a record's history
-- in order.
CREATE INDEX audit_subject_record ON audit_subject (resource, record_id, audit_id);
CREATE TRIGGER audit_subject_immutable BEFORE UPDATE OR DELETE ON audit_subject
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER audit_subject_no_truncate BEFORE TRUNCATE ON audit_subject
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
