-- Payment terms. A vendor names its usual terms; an order takes them from
-- its vendor unless it names others, and an invoice from its order. The
-- invoice's terms set the due date and early-payment discount of the open
-- item it posts.
ALTER TABLE vendor ADD COLUMN payment_terms_id uuid REFERENCES payment_terms (id);
ALTER TABLE purchase_order ADD COLUMN payment_terms_id uuid REFERENCES payment_terms (id);
ALTER TABLE vendor_invoice ADD COLUMN payment_terms_id uuid REFERENCES payment_terms (id);
