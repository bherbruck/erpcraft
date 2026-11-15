CREATE TABLE journal_entry_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    account_id uuid NOT NULL REFERENCES account (id),
    debit numeric(20, 4) NOT NULL DEFAULT 0 CHECK (debit >= 0),
    credit numeric(20, 4) NOT NULL DEFAULT 0 CHECK (credit >= 0),
    -- NULL: the company's accounting currency.
    currency text REFERENCES currency (code),
    fx_rate numeric(20, 10) CHECK (fx_rate > 0),
    memo text,
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER journal_entry_line_guard BEFORE INSERT OR UPDATE OR DELETE ON journal_entry_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
