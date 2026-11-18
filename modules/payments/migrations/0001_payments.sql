-- A payment settles open items of one party: the company pays a vendor's
-- invoices (outgoing), or a customer pays the company's (incoming). The
-- money moves through a bank or cash account of the general ledger.

CREATE TABLE payment (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    direction text NOT NULL CHECK (direction IN ('outgoing', 'incoming')),
    -- The bank or cash account the money leaves or arrives in.
    bank_account_id uuid NOT NULL REFERENCES account (id)
);

CREATE TABLE payment_allocation (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    amount numeric(20, 4) NOT NULL CHECK (amount > 0),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, open_item_id)
);

CREATE TRIGGER payment_guard BEFORE INSERT OR UPDATE OR DELETE ON payment
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER payment_allocation_guard BEFORE INSERT OR UPDATE OR DELETE ON payment_allocation
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
