-- An allocation may write off what is left of an item, or of a credit,
-- without any money: a customer who short-paid by 0.50 earlier, a credit
-- of 0.03 a customer overpaid. The company's policy payment.write_off
-- decides how much; nothing else refuses it. The write-off is a magnitude:
-- it runs the way of its item.
ALTER TABLE payment_allocation DROP CONSTRAINT payment_allocation_amount_check;
ALTER TABLE payment_allocation ADD CONSTRAINT payment_allocation_amount_check
    CHECK (amount <> 0 OR write_off > 0);

-- A payment may pay several parts of one item, each by name: the item
-- appears once per part. An allocation that names no part still takes an
-- item alone.
ALTER TABLE payment_allocation DROP CONSTRAINT payment_allocation_document_id_open_item_id_key;
CREATE UNIQUE INDEX payment_allocation_item_part
    ON payment_allocation (document_id, open_item_id, COALESCE(instalment, 0));
