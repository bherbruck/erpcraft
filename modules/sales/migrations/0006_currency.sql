-- Selling in a foreign currency. An order has a currency, fixed once it is
-- approved; its prices, and those of its invoices and credit notes, are in
-- it. NULL: the accounting currency. Deliveries and returns move stock,
-- which is valued in the accounting currency, so they carry no rate. An
-- invoice posts at its rate; a credit note at the rate of the invoice it
-- credits. fx_rate holds the rate stated or, once posted, posted at.
ALTER TABLE sales_order ADD COLUMN currency text REFERENCES currency (code);
ALTER TABLE sales_invoice ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);
ALTER TABLE sales_credit_note ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);
