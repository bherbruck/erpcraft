-- The company's logo, printed on its letterhead. An uploaded PNG or JPEG
-- kept here, not fetched from anywhere: a printed page and a PDF load
-- nothing from outside.
CREATE TABLE company_logo (
    company_id uuid PRIMARY KEY REFERENCES company (id) ON DELETE CASCADE,
    content_type text NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg')),
    data bytea NOT NULL CHECK (octet_length(data) BETWEEN 1 AND 524288),
    updated_at timestamptz NOT NULL DEFAULT now()
);
