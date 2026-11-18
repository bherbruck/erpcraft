-- Bank account numbers live in one place: banking's bank_account_number,
-- which bank files and e-invoices both read. The IBAN, BIC and account
-- holder kept here move there. An account banking has a record of keeps
-- its numbers; only a BIC or holder it lacks is filled in. Then this
-- module's table goes.
INSERT INTO bank_account_number (id, company_id, bank_account_id, iban, bic, account_name)
SELECT id, company_id, account_id, iban, bic, account_name FROM einvoice_bank_account
ON CONFLICT (bank_account_id) DO UPDATE
    SET bic = COALESCE(bank_account_number.bic, EXCLUDED.bic),
        account_name = COALESCE(bank_account_number.account_name, EXCLUDED.account_name);

DROP TABLE einvoice_bank_account;
DROP FUNCTION einvoice_bank_account_check();
