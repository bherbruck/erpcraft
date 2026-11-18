-- Charges: what a document can carry besides goods (freight, insurance,
-- handling, a service), each with the tax code it usually carries. Shared
-- by all companies, like tax codes; where a company posts a charge is an
-- account rule scoped to it (`charge_revenue` on sales, `charge_expense`
-- on purchases), else the company's default for the purpose.
CREATE TABLE charge (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    kind text NOT NULL DEFAULT 'other'
        CHECK (kind IN ('freight', 'insurance', 'handling', 'service', 'other')),
    tax_code_id uuid REFERENCES tax_code (id),
    -- On a vendor invoice: a cost of the goods received, to be allocated
    -- to them (landed cost), rather than an expense.
    landed_cost boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
