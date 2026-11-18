-- Goods go back to the vendor (a purchase return) and the vendor credits
-- what it charged (a vendor credit note). Both are documents of their own,
-- dated when they happen; a reversal still only undoes a mistake.
--
-- The accounting:
--   return:       credit inventory by what the goods leave stock at (the
--                 moving average), debit GRNI by what they were received
--                 for, the difference to purchase price variance;
--   credit note:  debit payables by what is credited, credit GRNI by what
--                 the credited goods were invoiced at (cleared), the
--                 difference to purchase price variance, and the tax back.
-- So GRNI holds goods received and not invoiced, less goods returned and
-- not credited, and clears when both sides are level.
--
-- An order line keeps what was received and invoiced in all, and what was
-- returned and credited apart. Net received is received less returned; net
-- invoiced is invoiced less credited.

ALTER TABLE purchase_order_progress
    ADD COLUMN qty_returned numeric(20, 6) NOT NULL DEFAULT 0,
    -- What the returned goods took out of GRNI.
    ADD COLUMN returned_value numeric(20, 4) NOT NULL DEFAULT 0,
    -- Of the returned goods, what the vendor has credited.
    ADD COLUMN qty_credited numeric(20, 6) NOT NULL DEFAULT 0,
    -- What those credits put back into GRNI.
    ADD COLUMN credited_value numeric(20, 4) NOT NULL DEFAULT 0;

ALTER TABLE purchase_order_progress DROP CONSTRAINT nothing_left_behind;
ALTER TABLE purchase_order_progress
    -- Only goods that were received go back, and only goods that went back
    -- (and were invoiced) are credited against GRNI.
    ADD CONSTRAINT returned_within_received CHECK (
        qty_returned >= 0 AND qty_returned <= qty_received
        AND returned_value >= 0 AND returned_value <= received_value),
    ADD CONSTRAINT credited_within_returned CHECK (
        qty_credited >= 0 AND qty_credited <= qty_returned AND qty_credited <= qty_invoiced
        AND credited_value >= 0 AND credited_value <= cleared_value),
    -- Once net received and net invoiced are level, nothing is left in GRNI
    -- for the line, and the other way round.
    ADD CONSTRAINT nothing_left_behind CHECK (
        qty_invoiced - qty_credited <> qty_received - qty_returned
        OR cleared_value - credited_value = received_value - returned_value);

-- ------------------------------------------------------------ purchase return

CREATE TABLE purchase_return (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    purchase_order_id uuid NOT NULL REFERENCES document (id)
);

CREATE TABLE purchase_return_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    order_line_id uuid NOT NULL REFERENCES purchase_order_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- Set when the return is posted: what the goods were received for,
    -- taken out of GRNI, and what they left stock at.
    value numeric(20, 4),
    stock_value numeric(20, 4),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, order_line_id)
);

CREATE TRIGGER purchase_return_guard BEFORE INSERT OR UPDATE OR DELETE ON purchase_return
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER purchase_return_line_guard BEFORE INSERT OR UPDATE OR DELETE ON purchase_return_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- -------------------------------------------------------- vendor credit note

CREATE TABLE vendor_credit_note (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    -- The invoice credited, and its order.
    vendor_invoice_id uuid NOT NULL REFERENCES document (id),
    purchase_order_id uuid NOT NULL REFERENCES document (id),
    -- The vendor's own number for the credit note.
    vendor_reference text
);

CREATE TABLE vendor_credit_note_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    invoice_line_id uuid NOT NULL REFERENCES vendor_invoice_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What is credited per unit.
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    -- Set when the credit note is posted: how much of the quantity is for
    -- returned goods, what that put back into GRNI, and the tax credited.
    returned_qty numeric(20, 6),
    cleared_value numeric(20, 4),
    tax_amount numeric(20, 4),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, invoice_line_id)
);
CREATE INDEX vendor_credit_note_line_invoice_line ON vendor_credit_note_line (invoice_line_id);

CREATE TRIGGER vendor_credit_note_guard BEFORE INSERT OR UPDATE OR DELETE ON vendor_credit_note
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER vendor_credit_note_line_guard BEFORE INSERT OR UPDATE OR DELETE ON vendor_credit_note_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
