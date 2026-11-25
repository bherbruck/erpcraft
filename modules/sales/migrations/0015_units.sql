-- A sales order line is in a unit of its item (`item_uom`), or in the
-- item's stock unit; it keeps the factor it was saved with: how many stock
-- units one of its unit is. Deliveries, returns and invoices follow it in
-- that unit; only the stock ledger and reservations are in stock units.
ALTER TABLE sales_order_line
    ADD COLUMN uom text CHECK (uom <> ''),
    ADD COLUMN factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (factor > 0);
