-- A landed-cost charge can also be shared out by the weight of the goods:
-- their quantity times their item's weight.
ALTER TABLE vendor_invoice_line DROP CONSTRAINT vendor_invoice_line_allocate_by_check;
ALTER TABLE vendor_invoice_line ADD CONSTRAINT vendor_invoice_line_allocate_by_check
    CHECK (allocate_by IN ('value', 'quantity', 'weight'));
