-- Stock transfer: moves stock between two warehouses of one company. The
-- stock leaves the source at its average cost there and arrives at the
-- destination with exactly that value.

CREATE TABLE stock_transfer (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    from_warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    to_warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    CHECK (from_warehouse_id <> to_warehouse_id)
);

CREATE TABLE stock_transfer_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER stock_transfer_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_transfer
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER stock_transfer_line_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_transfer_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
