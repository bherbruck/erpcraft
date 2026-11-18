-- What else a payer needs to know of a bank account: the bank's BIC and,
-- when it is not the company's own name, the name the account is held in.
-- E-invoices state both (BT-86, BT-85).
ALTER TABLE bank_account_number
    ADD COLUMN bic text CHECK (bic ~ '^[A-Z0-9]{8}([A-Z0-9]{3})?$'),
    ADD COLUMN account_name text CHECK (account_name <> '');
