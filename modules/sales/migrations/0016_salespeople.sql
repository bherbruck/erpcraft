-- Salespeople: who sells to a customer and on a document. See
-- "Salespeople and commissions" in ARCHITECTURE.md.
CREATE TABLE sales_rep (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    email text CHECK (email ~ '^[^@\s]+@[^@\s]+$'),
    -- The user the salesperson is, if any.
    user_id uuid REFERENCES app_user (id),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE customer ADD COLUMN salesperson_id uuid REFERENCES sales_rep (id);
ALTER TABLE sales_order ADD COLUMN salesperson_id uuid REFERENCES sales_rep (id);
ALTER TABLE sales_invoice ADD COLUMN salesperson_id uuid REFERENCES sales_rep (id);
ALTER TABLE sales_credit ADD COLUMN salesperson_id uuid REFERENCES sales_rep (id);
CREATE INDEX sales_invoice_salesperson ON sales_invoice (salesperson_id);
