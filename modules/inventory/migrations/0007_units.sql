-- Units of measure on stock adjustments and transfers: a line names a
-- unit of its item (item_uom; omitted, the stock unit) and keeps the
-- factor it was saved with. The line's quantity is kept in stock units, as
-- before, so the ledger, lots and what a transfer still has to ship are as
-- they were; the document shows and takes it in the line's unit. An
-- adjustment's unit cost is per the line's unit.
ALTER TABLE stock_adjustment_line
    ADD COLUMN uom text CHECK (uom <> ''),
    ADD COLUMN factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (factor > 0);
ALTER TABLE stock_transfer_line
    ADD COLUMN uom text CHECK (uom <> ''),
    ADD COLUMN factor numeric(20, 6) NOT NULL DEFAULT 1 CHECK (factor > 0);
