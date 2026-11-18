-- A small difference written off with a payment: what is left of an item
-- after the money and any early-payment discount, settled without being
-- paid (a customer short-paid by a few cents, a bank charge kept from a
-- transfer). Its policy point says how much a company accepts; it goes to
-- the account for `write_off`.
ALTER TABLE payment_allocation
    ADD COLUMN write_off numeric(20, 4) NOT NULL DEFAULT 0 CHECK (write_off >= 0);
