-- Year-end close: moves the balances of the income and expense accounts
-- over one fiscal period to a retained earnings account.

-- The content of the document. It can be changed while the document is a
-- draft.
CREATE TABLE year_end_close (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    -- The period being closed. It stands for the fiscal year.
    fiscal_period_id uuid NOT NULL REFERENCES fiscal_period (id),
    -- The equity account that receives the result.
    retained_earnings_account_id uuid NOT NULL REFERENCES account (id)
);

CREATE TRIGGER year_end_close_guard BEFORE INSERT OR UPDATE OR DELETE ON year_end_close
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- What a posted year-end close did. Kept beside the document, not in it,
-- because the reversal is recorded while the document's content is locked.
--
-- The closing posting is also recognizable without this table: its
-- posting.command is 'year_end_close.post', and that of its reversal is
-- 'year_end_close.reverse'. The general ledger's income statement leaves
-- out postings whose command starts with 'year_end_close.'.
CREATE TABLE year_end_closing (
    document_id uuid PRIMARY KEY REFERENCES document (id),
    fiscal_period_id uuid NOT NULL REFERENCES fiscal_period (id),
    -- The posting that brought income and expenses to zero.
    posting_id uuid NOT NULL UNIQUE REFERENCES posting (id),
    -- Set when the close is reversed.
    reversal_posting_id uuid UNIQUE REFERENCES posting (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A fiscal period has at most one year-end close in effect.
CREATE UNIQUE INDEX year_end_closing_one_per_period ON year_end_closing (fiscal_period_id)
    WHERE reversal_posting_id IS NULL;

-- A row is written once, and changed once at most: to record its reversal.
CREATE FUNCTION accounting_year_end_closing_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'the record of a year-end close cannot be deleted'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.document_id <> OLD.document_id OR NEW.fiscal_period_id <> OLD.fiscal_period_id
        OR NEW.posting_id <> OLD.posting_id OR OLD.reversal_posting_id IS NOT NULL
        OR NEW.reversal_posting_id IS NULL THEN
        RAISE EXCEPTION 'the record of a year-end close can only be marked as reversed, once'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER year_end_closing_guard BEFORE UPDATE OR DELETE ON year_end_closing
    FOR EACH ROW EXECUTE FUNCTION accounting_year_end_closing_guard();
