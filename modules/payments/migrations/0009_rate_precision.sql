-- Exchange rates are kept with 18 decimal places instead of 10 (numeric(28, 18):
-- up to 10 digits before the point, 28 in all, which is what the server's
-- decimal type holds exactly). Every amount in another currency is converted
-- by multiplying with one rate, and a rate recorded the other way round
-- (USD to JPY at 151.37, used for JPY to USD) is its inverse, rounded. At 10
-- places the inverse was off by enough to move large amounts by a cent
-- (151,370,000 JPY came to 1,000,000.01 USD); at 18 the error stays below
-- half a cent for any amount under about 10^13. A rate with more than 18
-- places is still refused, never rounded in.
--
-- ALTER COLUMN TYPE rewrites each table named here (and its indexes) under
-- an exclusive lock. Here: payments.

ALTER TABLE payment ALTER COLUMN fx_rate TYPE numeric(28, 18);
