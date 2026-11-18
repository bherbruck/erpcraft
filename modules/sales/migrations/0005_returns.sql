-- Goods come back from a customer (a sales return) and the customer is
-- credited for them (a sales credit note). Both are documents of their
-- own, dated when they happen; a reversal still only undoes a mistake.
--
-- The accounting:
--   return:       debit inventory, credit cost of goods sold, at the cost
--                 the goods left stock for (the average cost of what was
--                 delivered on the order line and not yet returned), so
--                 cost of goods sold is taken back exactly;
--   credit note:  debit revenue by what is credited, debit the tax back,
--                 credit receivables by both.
--
-- An order line keeps what was delivered and invoiced in all, and what came
-- back and was credited apart. Net delivered is delivered less returned;
-- net invoiced is invoiced less credited.

ALTER TABLE sales_order_progress
    ADD COLUMN qty_returned numeric(20, 6) NOT NULL DEFAULT 0,
    ADD COLUMN qty_credited numeric(20, 6) NOT NULL DEFAULT 0,
    -- Only goods that went out come back, and only goods that came back
    -- (and were invoiced) are credited as returned.
    ADD CONSTRAINT returned_within_delivered CHECK (qty_returned >= 0 AND qty_returned <= qty_delivered),
    ADD CONSTRAINT credited_within_returned CHECK (
        qty_credited >= 0 AND qty_credited <= qty_returned AND qty_credited <= qty_invoiced);

-- --------------------------------------------------------------- sales return

CREATE TABLE sales_return (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    sales_order_id uuid NOT NULL REFERENCES document (id)
);

CREATE TABLE sales_return_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    order_line_id uuid NOT NULL REFERENCES sales_order_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What the goods come back at: the cost they left for. Set when the
    -- return is posted.
    value numeric(20, 4) CHECK (value >= 0),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, order_line_id)
);

CREATE TRIGGER sales_return_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_return
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_return_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_return_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- --------------------------------------------------------- sales credit note

CREATE TABLE sales_credit_note (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    -- The invoice credited, and its order.
    sales_invoice_id uuid NOT NULL REFERENCES document (id),
    sales_order_id uuid NOT NULL REFERENCES document (id)
);

CREATE TABLE sales_credit_note_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    invoice_line_id uuid NOT NULL REFERENCES sales_invoice_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What is credited per unit.
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    -- Set when the credit note is posted: how much of the quantity is for
    -- goods that came back, and the tax credited.
    returned_qty numeric(20, 6),
    tax_amount numeric(20, 4),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, invoice_line_id)
);
CREATE INDEX sales_credit_note_line_invoice_line ON sales_credit_note_line (invoice_line_id);

CREATE TRIGGER sales_credit_note_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_credit_note
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_credit_note_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_credit_note_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
