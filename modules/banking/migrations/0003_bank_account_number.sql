-- The numbers the bank knows a bank account by: a statement file names one
-- of them, and is imported only into the account that has it.
CREATE TABLE bank_account_number (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    bank_account_id uuid NOT NULL UNIQUE REFERENCES account (id),
    -- Without spaces, in capitals.
    iban text CHECK (iban ~ '^[A-Z]{2}[0-9]{2}[A-Z0-9]{11,30}$'),
    -- The bank's own account number, as its files write it.
    account_number text CHECK (account_number <> ''),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (iban IS NOT NULL OR account_number IS NOT NULL)
);

-- A number is for a bank account of its own company.
CREATE FUNCTION bank_account_number_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM account WHERE id = NEW.bank_account_id
                   AND company_id = NEW.company_id AND is_bank) THEN
        RAISE EXCEPTION 'the bank account must be a bank or cash account of the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bank_account_number_check BEFORE INSERT OR UPDATE ON bank_account_number
    FOR EACH ROW EXECUTE FUNCTION bank_account_number_check();
