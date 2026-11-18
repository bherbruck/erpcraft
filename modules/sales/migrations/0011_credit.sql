-- A credit to a customer that is not tied to an invoice: a rebate over a
-- year's invoices, goodwill, a price agreed after the fact. It takes back
-- revenue (or charges another account) and the tax on it, at the codes its
-- lines name, and leaves the customer a credit: an open item below zero,
-- applied to its invoices like a credit note's.
CREATE TABLE sales_credit (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    customer_id uuid NOT NULL REFERENCES customer (id),
    -- NULL: the accounting currency.
    currency text REFERENCES currency (code),
    -- The rate stated or, once posted, posted at.
    fx_rate numeric(20, 10) CHECK (fx_rate > 0)
);

CREATE TABLE sales_credit_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    -- A charge (its revenue account) or an account, and what it is for.
    charge_id uuid REFERENCES charge (id),
    account_id uuid REFERENCES account (id),
    description text,
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    tax_code_id uuid REFERENCES tax_code (id),
    -- Set when the credit is posted.
    net_amount numeric(20, 4),
    tax_amount numeric(20, 4),
    UNIQUE (document_id, line_no),
    CONSTRAINT sales_credit_line_kind CHECK (num_nonnulls(charge_id, account_id) = 1)
);

CREATE TRIGGER sales_credit_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_credit
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_credit_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_credit_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
