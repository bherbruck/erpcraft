-- Purchasing in a foreign currency. An order has a currency, fixed once it
-- is approved (its content is locked then); its prices, and those of its
-- invoices and credit notes, are in it. NULL: the accounting currency.
--
-- Stock and goods received not invoiced are kept in the accounting
-- currency: a receipt converts at its own rate, and its line values stay
-- what the goods came into stock for. An invoice posts in the order's
-- currency at its rate; a credit note at the rate of the invoice it
-- credits. fx_rate holds the rate a document stated or, once posted, the
-- rate it was posted at.
ALTER TABLE purchase_order ADD COLUMN currency text REFERENCES currency (code);
ALTER TABLE purchase_receipt ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);
ALTER TABLE vendor_invoice ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);
ALTER TABLE vendor_credit_note ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);
