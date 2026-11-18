-- Which accounts hold money: a bank account, or cash. Payments offer only
-- these. Setup templates set it for the accounts they create; the
-- payments module marks the accounts that payments already used.
ALTER TABLE account ADD COLUMN is_bank boolean NOT NULL DEFAULT false;
