-- An invoice line can carry a tax code. The tax worked out when the invoice
-- is posted is kept on the line.

ALTER TABLE vendor_invoice_line
    ADD COLUMN tax_code_id uuid REFERENCES tax_code (id),
    ADD COLUMN tax_amount numeric(20, 4);
