-- The customer's addresses a document names: where its invoices go and
-- where its goods go. Nothing named: the customer's default of the kind.
-- Party data keeps the addresses; these are plain ids, checked when the
-- document is saved, since an address may later be removed while the
-- documents that named it keep the copy they were posted with.
ALTER TABLE sales_order ADD COLUMN billing_address_id uuid;
ALTER TABLE sales_order ADD COLUMN shipping_address_id uuid;
ALTER TABLE sales_invoice ADD COLUMN billing_address_id uuid;
ALTER TABLE sales_invoice ADD COLUMN shipping_address_id uuid;
ALTER TABLE sales_delivery ADD COLUMN shipping_address_id uuid;
