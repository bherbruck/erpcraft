-- A shipped transfer can be closed short: what never arrived is written
-- off from the transit warehouse, to the account for `transit_loss`.
ALTER TABLE stock_transfer_progress
    ADD COLUMN qty_written_off numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty_written_off >= 0),
    ADD CONSTRAINT stock_transfer_progress_within CHECK (qty_received + qty_written_off <= qty_shipped);
