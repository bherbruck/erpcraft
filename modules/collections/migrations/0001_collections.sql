-- Collections: customer statements, and dunning customers for what is
-- overdue. See "Collections: statements and dunning" in ARCHITECTURE.md.

-- ------------------------------------------------------------ statements

CREATE TABLE customer_statement (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    customer_id uuid NOT NULL REFERENCES customer (id),
    currency text NOT NULL REFERENCES currency (code),
    -- The period runs from this day to the statement's date.
    from_date date NOT NULL,
    -- In the statement's currency.
    opening numeric(20, 4) NOT NULL DEFAULT 0,
    closing numeric(20, 4) NOT NULL DEFAULT 0,
    -- What is open on the statement's date, by days past due.
    current_due numeric(20, 4) NOT NULL DEFAULT 0,
    overdue_1_30 numeric(20, 4) NOT NULL DEFAULT 0,
    overdue_31_60 numeric(20, 4) NOT NULL DEFAULT 0,
    overdue_61_90 numeric(20, 4) NOT NULL DEFAULT 0,
    overdue_over_90 numeric(20, 4) NOT NULL DEFAULT 0
);
CREATE INDEX customer_statement_customer ON customer_statement (customer_id);

-- What happened in the period: an item opened (invoice, credit) or a
-- posting that settled items (payment, credit applied, reversal).
CREATE TABLE customer_statement_activity (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    entry_date date NOT NULL,
    kind text NOT NULL CHECK (kind IN ('item', 'settlement', 'cancelled')),
    source_doc_type text,
    source_document_id uuid,
    number text,
    debit numeric(20, 4) NOT NULL DEFAULT 0,
    credit numeric(20, 4) NOT NULL DEFAULT 0,
    -- The balance after this line.
    balance numeric(20, 4) NOT NULL,
    UNIQUE (document_id, line_no)
);

-- The items open on the statement's date.
CREATE TABLE customer_statement_item (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    source_doc_type text,
    source_document_id uuid,
    number text,
    doc_date date NOT NULL,
    due_date date,
    days_overdue integer NOT NULL,
    amount numeric(20, 4) NOT NULL,
    open numeric(20, 4) NOT NULL,
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER customer_statement_guard BEFORE INSERT OR UPDATE OR DELETE ON customer_statement
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER customer_statement_activity_guard
    BEFORE INSERT OR UPDATE OR DELETE ON customer_statement_activity
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER customer_statement_item_guard
    BEFORE INSERT OR UPDATE OR DELETE ON customer_statement_item
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- --------------------------------------------------------------- dunning

CREATE TABLE dunning_level (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    level integer NOT NULL CHECK (level BETWEEN 1 AND 9),
    name text NOT NULL CHECK (name <> ''),
    -- An item reaches the level this many days after it fell due.
    days_overdue integer NOT NULL CHECK (days_overdue >= 0),
    -- Per letter, in the accounting currency.
    fee numeric(20, 4) NOT NULL DEFAULT 0 CHECK (fee >= 0),
    fee_account_id uuid REFERENCES account (id),
    fee_taxable boolean NOT NULL DEFAULT false,
    -- Percent a year, on what is overdue, per day on a 365-day year.
    interest_rate numeric(9, 6) NOT NULL DEFAULT 0 CHECK (interest_rate BETWEEN 0 AND 100),
    interest_account_id uuid REFERENCES account (id),
    -- What the letter charges is due this many days after it.
    pay_within_days integer NOT NULL DEFAULT 10 CHECK (pay_within_days >= 0),
    subject text CHECK (subject <> ''),
    text text CHECK (text <> ''),
    print_template_id uuid REFERENCES print_template (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, level),
    CONSTRAINT dunning_level_fee_account CHECK (fee = 0 OR fee_account_id IS NOT NULL),
    CONSTRAINT dunning_level_interest_account
        CHECK (interest_rate = 0 OR interest_account_id IS NOT NULL)
);

-- The accounts are the company's, and a higher level is reached later.
CREATE FUNCTION dunning_level_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM account
               WHERE id IN (NEW.fee_account_id, NEW.interest_account_id)
                 AND company_id <> NEW.company_id) THEN
        RAISE EXCEPTION 'the accounts of a dunning level must be its company''s'
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (SELECT 1 FROM dunning_level
               WHERE company_id = NEW.company_id AND id <> NEW.id
                 AND ((level < NEW.level AND days_overdue >= NEW.days_overdue)
                      OR (level > NEW.level AND days_overdue <= NEW.days_overdue))) THEN
        RAISE EXCEPTION 'a higher dunning level is reached more days overdue than a lower one'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER dunning_level_check BEFORE INSERT OR UPDATE ON dunning_level
    FOR EACH ROW EXECUTE FUNCTION dunning_level_check();

-- A customer, or one item, not to be dunned.
CREATE TABLE dunning_hold (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    customer_id uuid REFERENCES customer (id),
    open_item_id uuid REFERENCES open_item (id),
    reason text NOT NULL CHECK (reason <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((customer_id IS NULL) <> (open_item_id IS NULL))
);
CREATE INDEX dunning_hold_company ON dunning_hold (company_id) WHERE active;

-- A customer paying what it owes by agreement: not dunned while active.
CREATE TABLE payment_plan (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    customer_id uuid NOT NULL REFERENCES customer (id),
    starts_on date NOT NULL,
    ends_on date,
    note text,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_on IS NULL OR ends_on >= starts_on)
);
CREATE INDEX payment_plan_customer ON payment_plan (company_id, customer_id) WHERE active;

-- The run: what is proposed, reviewed and posted.
CREATE TABLE dunning_run_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    customer_id uuid NOT NULL REFERENCES customer (id),
    due_date date,
    days_overdue integer NOT NULL,
    -- What is open, in the item's currency, and what it is carried at.
    currency text NOT NULL,
    open numeric(20, 4) NOT NULL,
    carried numeric(20, 4) NOT NULL,
    level integer NOT NULL CHECK (level BETWEEN 1 AND 9),
    -- In the accounting currency, from this day to the run's date.
    interest_from date,
    proposed_interest numeric(20, 4) NOT NULL DEFAULT 0,
    interest numeric(20, 4) NOT NULL DEFAULT 0 CHECK (interest >= 0),
    skip boolean NOT NULL DEFAULT false,
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, open_item_id)
);
CREATE INDEX dunning_run_line_item ON dunning_run_line (open_item_id);

CREATE TABLE dunning_run_letter (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    customer_id uuid NOT NULL REFERENCES customer (id),
    level integer NOT NULL CHECK (level BETWEEN 1 AND 9),
    -- In the accounting currency.
    proposed_fee numeric(20, 4) NOT NULL DEFAULT 0,
    fee numeric(20, 4) NOT NULL DEFAULT 0 CHECK (fee >= 0),
    skip boolean NOT NULL DEFAULT false,
    -- The letter posting made.
    letter_id uuid REFERENCES document (id),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, customer_id)
);

-- A letter: what one customer is told and charged.
CREATE TABLE dunning_letter (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    run_id uuid REFERENCES document (id),
    customer_id uuid NOT NULL REFERENCES customer (id),
    level integer NOT NULL CHECK (level BETWEEN 1 AND 9),
    -- In the accounting currency.
    fee numeric(20, 4) NOT NULL DEFAULT 0,
    tax numeric(20, 4) NOT NULL DEFAULT 0,
    interest numeric(20, 4) NOT NULL DEFAULT 0,
    due_date date,
    subject text,
    text text,
    print_template_id uuid REFERENCES print_template (id)
);
CREATE INDEX dunning_letter_customer ON dunning_letter (customer_id);

CREATE TABLE dunning_letter_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    number text,
    doc_date date NOT NULL,
    due_date date,
    days_overdue integer NOT NULL,
    currency text NOT NULL,
    open numeric(20, 4) NOT NULL,
    level integer NOT NULL,
    interest numeric(20, 4) NOT NULL DEFAULT 0,
    interest_from date,
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER dunning_run_line_guard BEFORE INSERT OR UPDATE OR DELETE ON dunning_run_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER dunning_run_letter_guard BEFORE INSERT OR UPDATE OR DELETE ON dunning_run_letter
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER dunning_letter_guard BEFORE INSERT OR UPDATE OR DELETE ON dunning_letter
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER dunning_letter_line_guard BEFORE INSERT OR UPDATE OR DELETE ON dunning_letter_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- Each level an item reached, with the letter that told the customer and
-- the interest charged. Written by posting a run, never changed.
CREATE TABLE dunning_history (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    customer_id uuid NOT NULL REFERENCES customer (id),
    letter_id uuid NOT NULL REFERENCES document (id),
    run_id uuid NOT NULL REFERENCES document (id),
    level integer NOT NULL,
    dunned_on date NOT NULL,
    interest numeric(20, 4) NOT NULL DEFAULT 0,
    -- The days the interest covers.
    interest_from date,
    interest_to date,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- An item reaches each level once.
    UNIQUE (open_item_id, level)
);
CREATE INDEX dunning_history_customer ON dunning_history (company_id, customer_id);
CREATE TRIGGER dunning_history_immutable BEFORE UPDATE OR DELETE ON dunning_history
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
