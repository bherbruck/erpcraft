-- General ledger. The rules here hold no matter which module writes:
-- they are enforced by the database, not by the caller.

CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE TABLE account (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    account_type text NOT NULL CHECK (account_type IN ('asset', 'liability', 'equity', 'income', 'expense')),
    parent_id uuid REFERENCES account (id),
    -- A group account only organizes the chart; it cannot be posted to.
    is_group boolean NOT NULL DEFAULT false,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code)
);

CREATE FUNCTION gl_account_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.parent_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM account WHERE id = NEW.parent_id AND company_id = NEW.company_id
    ) THEN
        RAISE EXCEPTION 'the parent account must belong to the same company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND NEW.is_group AND NOT OLD.is_group
        AND EXISTS (SELECT 1 FROM gl_entry WHERE account_id = NEW.id) THEN
        RAISE EXCEPTION 'account % has entries and cannot become a group account', NEW.code
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

CREATE TABLE fiscal_period (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    name text NOT NULL CHECK (name <> ''),
    start_date date NOT NULL,
    end_date date NOT NULL,
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (end_date >= start_date),
    UNIQUE (company_id, name),
    CONSTRAINT fiscal_period_no_overlap
        EXCLUDE USING gist (company_id WITH =, daterange(start_date, end_date, '[]') WITH &&)
);

-- One side of a posting in the general ledger.
--
-- txn_debit / txn_credit are in the transaction currency. debit / credit
-- are the same amount in the company's accounting currency, converted at
-- fx_rate and rounded to that currency's decimals. A posting balances in
-- the accounting currency.
CREATE TABLE gl_entry (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    posting_id uuid NOT NULL REFERENCES posting (id),
    line_no integer NOT NULL,
    -- The document line that caused this entry, when there is one.
    source_line_id uuid,
    account_id uuid NOT NULL REFERENCES account (id),
    posting_date date NOT NULL,
    debit numeric(20, 4) NOT NULL CHECK (debit >= 0),
    credit numeric(20, 4) NOT NULL CHECK (credit >= 0),
    currency text NOT NULL REFERENCES currency (code),
    txn_debit numeric(20, 4) NOT NULL CHECK (txn_debit >= 0),
    txn_credit numeric(20, 4) NOT NULL CHECK (txn_credit >= 0),
    fx_rate numeric(20, 10) NOT NULL CHECK (fx_rate > 0),
    memo text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (posting_id, line_no),
    -- Exactly one side, and the accounting amount on the same side.
    CHECK ((txn_debit = 0) <> (txn_credit = 0)),
    CHECK ((txn_debit = 0 OR credit = 0) AND (txn_credit = 0 OR debit = 0))
);
CREATE INDEX gl_entry_account_date ON gl_entry (account_id, posting_date);
CREATE INDEX gl_entry_company_date ON gl_entry (company_id, posting_date);

CREATE TRIGGER account_check BEFORE INSERT OR UPDATE ON account
    FOR EACH ROW EXECUTE FUNCTION gl_account_check();

CREATE FUNCTION gl_entry_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    acc account%ROWTYPE;
    base text;
    base_decimals integer;
    txn_decimals integer;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    IF pst.company_id <> NEW.company_id OR pst.posting_date <> NEW.posting_date THEN
        RAISE EXCEPTION 'a ledger entry must carry the company and date of its posting'
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT * INTO acc FROM account WHERE id = NEW.account_id;
    IF acc.company_id <> NEW.company_id THEN
        RAISE EXCEPTION 'account % belongs to another company', acc.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF acc.is_group THEN
        RAISE EXCEPTION 'account % is a group account and cannot be posted to', acc.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT acc.active THEN
        RAISE EXCEPTION 'account % is inactive', acc.code USING ERRCODE = 'check_violation';
    END IF;

    -- FOR SHARE makes posting and closing a period wait for each other: a
    -- close that commits first is seen here, and a close that starts later
    -- waits until this transaction ends.
    PERFORM 1 FROM fiscal_period
        WHERE company_id = NEW.company_id AND status = 'open'
          AND NEW.posting_date BETWEEN start_date AND end_date
        FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no open fiscal period contains %', NEW.posting_date
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT c.base_currency, cur.decimals INTO base, base_decimals
        FROM company c JOIN currency cur ON cur.code = c.base_currency
        WHERE c.id = NEW.company_id;
    SELECT decimals INTO txn_decimals FROM currency WHERE code = NEW.currency;

    IF NEW.txn_debit <> round(NEW.txn_debit, txn_decimals)
        OR NEW.txn_credit <> round(NEW.txn_credit, txn_decimals) THEN
        RAISE EXCEPTION 'an amount has more decimal places than % allows', NEW.currency
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.currency = base AND NEW.fx_rate <> 1 THEN
        RAISE EXCEPTION 'an entry in the accounting currency must have an exchange rate of 1'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.debit <> round(NEW.txn_debit * NEW.fx_rate, base_decimals)
        OR NEW.credit <> round(NEW.txn_credit * NEW.fx_rate, base_decimals) THEN
        RAISE EXCEPTION 'the accounting amount must equal the transaction amount times the exchange rate, rounded to % decimals', base_decimals
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER gl_entry_before_insert BEFORE INSERT ON gl_entry
    FOR EACH ROW EXECUTE FUNCTION gl_entry_before_insert();

CREATE TRIGGER gl_entry_immutable BEFORE UPDATE OR DELETE ON gl_entry
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER gl_entry_no_truncate BEFORE TRUNCATE ON gl_entry
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- Checked when the transaction commits, once all entries of the posting
-- are written.
CREATE FUNCTION gl_posting_balanced() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    total_debit numeric;
    total_credit numeric;
BEGIN
    SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0) INTO total_debit, total_credit
        FROM gl_entry WHERE posting_id = NEW.posting_id;
    IF total_debit <> total_credit THEN
        RAISE EXCEPTION 'posting % does not balance: debit % is not equal to credit %',
            NEW.posting_id, total_debit, total_credit
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER gl_posting_balanced AFTER INSERT ON gl_entry
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION gl_posting_balanced();
