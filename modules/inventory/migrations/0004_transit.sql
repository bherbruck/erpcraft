-- Transfers in two steps, through a warehouse that holds goods in transit.
--
-- A transfer that names a transit warehouse ships: the goods leave the
-- source at their average cost and come into the transit warehouse with
-- that value. Transfer receipts then take them, in parts if need be, from
-- the transit warehouse into the destination. How much of each line has
-- arrived is kept beside the transfer, whose content is locked.

ALTER TABLE stock_transfer ADD COLUMN transit_warehouse_id uuid REFERENCES warehouse (id);

CREATE TABLE stock_transfer_progress (
    line_id uuid PRIMARY KEY REFERENCES stock_transfer_line (id),
    document_id uuid NOT NULL REFERENCES document (id),
    qty_shipped numeric(20, 6) NOT NULL CHECK (qty_shipped > 0),
    qty_received numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty_received >= 0),
    -- Nothing arrives that was not shipped.
    CHECK (qty_received <= qty_shipped)
);
CREATE INDEX stock_transfer_progress_document ON stock_transfer_progress (document_id);

CREATE TABLE stock_transfer_receipt (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    transfer_id uuid NOT NULL REFERENCES document (id)
);
CREATE INDEX stock_transfer_receipt_transfer ON stock_transfer_receipt (transfer_id);

CREATE TABLE stock_transfer_receipt_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    transfer_line_id uuid NOT NULL REFERENCES stock_transfer_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- Set when the receipt is posted: the value that arrived.
    value numeric(20, 4),
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER stock_transfer_receipt_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_transfer_receipt
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER stock_transfer_receipt_line_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_transfer_receipt_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
