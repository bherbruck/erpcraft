-- Sending documents by email. Each company names the SMTP server it sends
-- through; the password is kept apart, in a table no generic read
-- reaches, and is never returned. A document sent is recorded here with
-- what went out: to whom, the subject and text, the PDF, who sent it,
-- when it left and the Message-ID it left with. The sending itself is
-- done after the command commits, by the outbox, which retries.
CREATE TABLE smtp_settings (
    company_id uuid PRIMARY KEY REFERENCES company (id) ON DELETE CASCADE,
    host text NOT NULL CHECK (host <> ''),
    port integer NOT NULL CHECK (port BETWEEN 1 AND 65535),
    -- starttls: upgrade a plain connection (port 587); tls: TLS from the
    -- start (port 465); none: no encryption, for a relay on the same host
    -- or network only.
    security text NOT NULL DEFAULT 'starttls' CHECK (security IN ('starttls', 'tls', 'none')),
    username text CHECK (username <> ''),
    from_address text NOT NULL CHECK (from_address LIKE '%_@_%'),
    from_name text CHECK (from_name <> ''),
    reply_to text CHECK (reply_to LIKE '%_@_%'),
    -- Copied, unseen, on every message: the company's own record.
    bcc text CHECK (bcc LIKE '%_@_%'),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE smtp_secret (
    company_id uuid PRIMARY KEY REFERENCES smtp_settings (company_id) ON DELETE CASCADE,
    password text NOT NULL CHECK (password <> '')
);

CREATE TABLE document_message (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    document_id uuid NOT NULL REFERENCES document (id),
    doc_type text NOT NULL,
    to_addresses text[] NOT NULL CHECK (cardinality(to_addresses) > 0),
    cc_addresses text[] NOT NULL DEFAULT '{}',
    subject text NOT NULL,
    body text NOT NULL,
    attachment_name text NOT NULL,
    attachment bytea NOT NULL,
    -- queued: waiting to leave; sent: the server took it; failed: given
    -- up after too many attempts.
    status text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'sent', 'failed')),
    attempts integer NOT NULL DEFAULT 0,
    last_error text,
    message_id text NOT NULL UNIQUE,
    sent_by uuid NOT NULL REFERENCES app_user (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz
);
CREATE INDEX document_message_document ON document_message (document_id, created_at);
