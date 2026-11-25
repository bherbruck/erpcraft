-- A purchase order line is in a unit of its item (`item_uom`), or in the
-- item's stock unit; it keeps the factor it was saved with: how many stock
-- units one of its unit is. Receipts and returns follow it in that unit;
-- only the stock ledger is given stock units.
ALTER TABLE purchase_order_line
    ADD COLUMN uom text CHECK (uom <> ''),
    ADD COLUMN factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (factor > 0);
