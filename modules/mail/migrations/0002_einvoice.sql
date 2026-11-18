-- What a message carried: a PDF, a ZUGFeRD PDF or an XRechnung XML, and
-- the attachment's media type.
ALTER TABLE document_message ADD COLUMN format text NOT NULL DEFAULT 'pdf'
    CHECK (format IN ('pdf', 'zugferd', 'xrechnung_ubl', 'xrechnung_cii'));
ALTER TABLE document_message ADD COLUMN attachment_type text NOT NULL DEFAULT 'application/pdf';
