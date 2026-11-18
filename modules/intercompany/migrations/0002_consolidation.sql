-- A group of companies reported together in one currency. Each member is
-- wholly owned. The consolidation is a report: nothing here holds figures.
CREATE TABLE consolidation_group (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    currency text NOT NULL REFERENCES currency (code),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE consolidation_member (
    id uuid PRIMARY KEY,
    group_id uuid NOT NULL REFERENCES consolidation_group (id) ON DELETE CASCADE,
    company_id uuid NOT NULL REFERENCES company (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (group_id, company_id)
);
