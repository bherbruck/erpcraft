-- An account can hold money in one currency: a bank account in euros. A
-- payment through it is in that currency. NULL: no currency of its own;
-- such an account takes entries in any currency, as before.
ALTER TABLE account ADD COLUMN currency text REFERENCES currency (code);
