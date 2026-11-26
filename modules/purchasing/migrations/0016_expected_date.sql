-- When the goods of a purchase order are expected: what time-phased
-- planning dates its open quantity by. Left out, planning takes the
-- order's date plus the vendor's lead time.
ALTER TABLE purchase_order ADD COLUMN expected_date date;
