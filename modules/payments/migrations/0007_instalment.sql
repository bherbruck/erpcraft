-- The part of an item due in parts that an allocation pays. Nothing named:
-- the earliest part still open first.
ALTER TABLE payment_allocation ADD COLUMN instalment integer CHECK (instalment >= 1);
