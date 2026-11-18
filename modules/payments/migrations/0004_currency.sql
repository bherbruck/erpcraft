-- A payment in a foreign currency. Its allocations are in that currency,
-- which is the currency of every open item it settles; its bank line is in
-- that currency at the payment's rate. NULL: the accounting currency.
-- fx_rate is the rate stated on the payment, or, once posted, the rate it
-- was posted at.
ALTER TABLE payment
    ADD COLUMN currency text REFERENCES currency (code),
    ADD COLUMN fx_rate numeric(20, 10) CHECK (fx_rate > 0);
