-- Bank statements: what the bank says moved through a bank account, to be
-- reconciled with what the books say.

CREATE TABLE bank_statement (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    -- An is_bank account of the document's company.
    bank_account_id uuid NOT NULL REFERENCES account (id),
    -- The account's currency: the statement is in it.
    currency text NOT NULL REFERENCES currency (code),
    period_start date NOT NULL,
    period_end date NOT NULL,
    opening_balance numeric(20, 4) NOT NULL,
    closing_balance numeric(20, 4) NOT NULL,
    -- How the statement came in: csv, camt053, ofx, or null when typed in.
    source_format text,
    CHECK (period_end >= period_start)
);
CREATE INDEX bank_statement_account ON bank_statement (bank_account_id, period_end);

CREATE TRIGGER bank_statement_guard BEFORE INSERT OR UPDATE OR DELETE ON bank_statement
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

CREATE TABLE bank_statement_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    bank_account_id uuid NOT NULL REFERENCES account (id),
    booking_date date NOT NULL,
    value_date date,
    -- Positive: money in. Negative: money out. In the statement's currency.
    amount numeric(20, 4) NOT NULL CHECK (amount <> 0),
    counterparty_name text,
    counterparty_account text,
    reference text,
    -- The bank's own id of the transaction: the same file imported twice
    -- creates nothing twice.
    bank_transaction_id text,
    -- What the line is: a posting that moved the bank account (a payment,
    -- or this statement's own posting of a fee), a payment drafted for it
    -- and not posted yet, or an account to post it to when the statement
    -- is posted.
    posting_id uuid REFERENCES posting (id),
    payment_id uuid REFERENCES document (id),
    account_id uuid REFERENCES account (id),
    -- Part of the line that differs from the payment it was matched to,
    -- posted to account_id when the statement is posted.
    difference numeric(20, 4) NOT NULL DEFAULT 0,
    -- The statement's own posting of the difference, once posted.
    difference_posting_id uuid REFERENCES posting (id),
    UNIQUE (document_id, line_no)
);
CREATE UNIQUE INDEX bank_statement_line_transaction
    ON bank_statement_line (bank_account_id, bank_transaction_id)
    WHERE bank_transaction_id IS NOT NULL;
CREATE INDEX bank_statement_line_posting ON bank_statement_line (posting_id);

CREATE TRIGGER bank_statement_line_guard BEFORE INSERT OR UPDATE OR DELETE ON bank_statement_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- How one bank's CSV export reads, saved per bank account so that it is
-- mapped once. A column is named by its header, or by its position from 1
-- when the file has no header.
CREATE TABLE bank_csv_mapping (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    bank_account_id uuid NOT NULL UNIQUE REFERENCES account (id),
    delimiter text NOT NULL DEFAULT ',' CHECK (length(delimiter) = 1),
    has_header boolean NOT NULL DEFAULT true,
    date_format text NOT NULL DEFAULT '%Y-%m-%d',
    decimal_comma boolean NOT NULL DEFAULT false,
    booking_date_column text NOT NULL,
    value_date_column text,
    -- Either one signed amount column, or a debit and a credit column.
    amount_column text,
    debit_column text,
    credit_column text,
    counterparty_name_column text,
    counterparty_account_column text,
    reference_column text,
    transaction_id_column text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (amount_column IS NOT NULL OR (debit_column IS NOT NULL AND credit_column IS NOT NULL))
);

-- A mapping is for a bank account of its own company.
CREATE FUNCTION bank_csv_mapping_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM account WHERE id = NEW.bank_account_id
                   AND company_id = NEW.company_id AND is_bank) THEN
        RAISE EXCEPTION 'the bank account must be a bank or cash account of the mapping''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bank_csv_mapping_check BEFORE INSERT OR UPDATE ON bank_csv_mapping
    FOR EACH ROW EXECUTE FUNCTION bank_csv_mapping_check();
