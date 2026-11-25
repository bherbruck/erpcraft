-- Sales quotes: prices offered to a customer before it orders. See "Sales
-- quotes" in ARCHITECTURE.md.

CREATE TABLE sales_quote (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    customer_id uuid NOT NULL REFERENCES customer (id),
    -- None: the accounting currency.
    currency text REFERENCES currency (code),
    valid_until date NOT NULL
);
CREATE INDEX sales_quote_customer ON sales_quote (customer_id);

CREATE TABLE sales_quote_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid REFERENCES item (id),
    warehouse_id uuid REFERENCES warehouse (id),
    charge_id uuid REFERENCES charge (id),
    account_id uuid REFERENCES account (id),
    description text,
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    discount_percent numeric(9, 6) CHECK (discount_percent > 0 AND discount_percent <= 100),
    discount_amount numeric(20, 4) CHECK (discount_amount > 0),
    UNIQUE (document_id, line_no),
    CONSTRAINT sales_quote_line_kind CHECK (
        (item_id IS NOT NULL AND warehouse_id IS NOT NULL AND charge_id IS NULL AND account_id IS NULL)
        OR (item_id IS NULL AND warehouse_id IS NULL AND num_nonnulls(charge_id, account_id) = 1))
);

CREATE TRIGGER sales_quote_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_quote
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_quote_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_quote_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- How a quote ended: won into an order, or lost, and why. Written once,
-- after the quote is locked, so kept apart from it.
CREATE TABLE sales_quote_outcome (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    outcome text NOT NULL CHECK (outcome IN ('won', 'lost')),
    reason text CHECK (reason <> ''),
    order_id uuid REFERENCES document (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (outcome = 'won' OR (reason IS NOT NULL AND order_id IS NULL))
);
CREATE TRIGGER sales_quote_outcome_immutable BEFORE UPDATE ON sales_quote_outcome
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
