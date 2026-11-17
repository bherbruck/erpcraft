-- Stock adjustment: stock found, lost, written off, or entered as an
-- opening balance. Its value goes to the general ledger between the
-- inventory account and an offset account.

CREATE TABLE stock_adjustment (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    -- The asset account that carries the stock value.
    inventory_account_id uuid REFERENCES account (id),
    -- Where the other side goes: a shrinkage expense, opening equity.
    offset_account_id uuid REFERENCES account (id)
);

CREATE TABLE stock_adjustment_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    -- Positive: stock added. Negative: stock removed.
    qty numeric(20, 6) NOT NULL CHECK (qty <> 0),
    -- For stock added only. Stock removed leaves at the average cost.
    unit_cost numeric(20, 6) CHECK (unit_cost >= 0),
    UNIQUE (document_id, line_no),
    CHECK ((qty > 0) = (unit_cost IS NOT NULL))
);

CREATE TRIGGER stock_adjustment_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_adjustment
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER stock_adjustment_line_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_adjustment_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
