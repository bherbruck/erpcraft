-- Print templates: how a document looks on paper. Configuration, not code:
-- an administrator changes them without a rebuild.

CREATE TABLE print_template (
    id uuid PRIMARY KEY,
    -- A registered document type, such as sales_invoice. Checked when the
    -- template is saved.
    doc_type text NOT NULL CHECK (doc_type <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- Only for this company's documents. Null: for every company's.
    company_id uuid REFERENCES company (id),
    -- HTML with MiniJinja tags. Compiled and checked when saved.
    body text NOT NULL CHECK (body <> ''),
    -- Used when no template is asked for.
    is_default boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- One default per document type for each company, and one for all
-- companies: null counts as one scope of its own.
CREATE UNIQUE INDEX print_template_default
    ON print_template (doc_type, company_id) NULLS NOT DISTINCT
    WHERE is_default;
CREATE INDEX print_template_doc_type ON print_template (doc_type);
