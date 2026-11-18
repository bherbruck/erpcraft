-- The vendor's address a document names: where it sells from and its
-- invoices come from. Nothing named: the vendor's default billing
-- address. Party data keeps the addresses; these are plain ids, checked
-- when the document is saved.
ALTER TABLE purchase_order ADD COLUMN billing_address_id uuid;
ALTER TABLE vendor_invoice ADD COLUMN billing_address_id uuid;
