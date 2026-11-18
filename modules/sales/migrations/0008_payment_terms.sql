-- Payment terms. A customer names its usual terms; an order takes them
-- from its customer unless it names others, and an invoice from its
-- order. The invoice's terms set the due date and early-payment discount
-- of the open item it posts.
ALTER TABLE customer ADD COLUMN payment_terms_id uuid REFERENCES payment_terms (id);
ALTER TABLE sales_order ADD COLUMN payment_terms_id uuid REFERENCES payment_terms (id);
ALTER TABLE sales_invoice ADD COLUMN payment_terms_id uuid REFERENCES payment_terms (id);
