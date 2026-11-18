-- A stock adjustment no longer names its inventory account: the general
-- ledger's account rules say which account each item in each warehouse
-- posts to. It keeps its offset account.

ALTER TABLE stock_adjustment DROP COLUMN inventory_account_id;
