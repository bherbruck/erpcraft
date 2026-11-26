-- Commission: plans, runs that accrue what salespeople earned, and their
-- statements. See "Salespeople and commissions" in ARCHITECTURE.md.

CREATE TABLE commission_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    expense_account_id uuid NOT NULL REFERENCES account (id),
    payable_account_id uuid NOT NULL REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A salesperson paid as a vendor: its commission is an open payable.
CREATE TABLE commission_payee (
    id uuid PRIMARY KEY,
    sales_rep_id uuid NOT NULL UNIQUE REFERENCES sales_rep (id),
    vendor_id uuid NOT NULL REFERENCES vendor (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE commission_plan (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    basis text NOT NULL DEFAULT 'net' CHECK (basis IN ('net', 'margin')),
    trigger text NOT NULL DEFAULT 'invoice' CHECK (trigger IN ('invoice', 'payment')),
    sales_rep_id uuid REFERENCES sales_rep (id),
    item_group_id uuid REFERENCES price_group (id),
    valid_from date,
    valid_to date,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to >= valid_from)
);

CREATE TABLE commission_tier (
    id uuid PRIMARY KEY,
    plan_id uuid NOT NULL REFERENCES commission_plan (id) ON DELETE CASCADE,
    company_id uuid NOT NULL REFERENCES company (id),
    from_amount numeric(20, 4) NOT NULL DEFAULT 0 CHECK (from_amount >= 0),
    percent numeric(9, 4) NOT NULL CHECK (percent >= 0 AND percent <= 100),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (plan_id, from_amount)
);

CREATE FUNCTION commission_tier_company() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.company_id IS NULL THEN
        NEW.company_id := (SELECT company_id FROM commission_plan WHERE id = NEW.plan_id);
    ELSIF NEW.company_id IS DISTINCT FROM (SELECT company_id FROM commission_plan WHERE id = NEW.plan_id) THEN
        RAISE EXCEPTION 'a commission tier is of its plan''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER commission_tier_company BEFORE INSERT OR UPDATE ON commission_tier
    FOR EACH ROW EXECUTE FUNCTION commission_tier_company();

-- The run.
CREATE TABLE commission_run (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    from_date date NOT NULL,
    to_date date NOT NULL,
    CHECK (to_date >= from_date)
);
CREATE TRIGGER commission_run_guard BEFORE INSERT OR UPDATE OR DELETE ON commission_run
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- Per salesperson and plan: what was earned, and the commission.
CREATE TABLE commission_run_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    sales_rep_id uuid NOT NULL REFERENCES sales_rep (id),
    plan_id uuid NOT NULL REFERENCES commission_plan (id),
    basis numeric(20, 4) NOT NULL,
    proposed numeric(20, 4) NOT NULL,
    commission numeric(20, 4) NOT NULL,
    statement_id uuid REFERENCES document (id),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, sales_rep_id, plan_id)
);
CREATE TRIGGER commission_run_line_guard BEFORE INSERT OR UPDATE OR DELETE ON commission_run_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- Each line of a sale, credit or payment that earned or took back.
CREATE TABLE commission_item (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    sales_rep_id uuid NOT NULL REFERENCES sales_rep (id),
    plan_id uuid NOT NULL REFERENCES commission_plan (id),
    -- The sales document, the posting and the line.
    source_document_id uuid NOT NULL REFERENCES document (id),
    source_posting_id uuid NOT NULL REFERENCES posting (id),
    source_line_id uuid NOT NULL,
    -- For a plan on payment: the settlement that earned it.
    settlement_id uuid REFERENCES settlement (id),
    earned_on date NOT NULL,
    basis numeric(20, 4) NOT NULL,
    commission numeric(20, 4) NOT NULL,
    dimension_set_id uuid,
    UNIQUE (document_id, source_posting_id, source_line_id, settlement_id)
);
CREATE INDEX commission_item_source ON commission_item (source_posting_id, source_line_id);
CREATE TRIGGER commission_item_guard BEFORE INSERT OR UPDATE OR DELETE ON commission_item
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- A salesperson's statement of a run.
CREATE TABLE commission_statement (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    run_id uuid NOT NULL REFERENCES document (id),
    sales_rep_id uuid NOT NULL REFERENCES sales_rep (id),
    vendor_id uuid REFERENCES vendor (id),
    from_date date NOT NULL,
    to_date date NOT NULL,
    total numeric(20, 4) NOT NULL
);
CREATE TRIGGER commission_statement_guard BEFORE INSERT OR UPDATE OR DELETE ON commission_statement
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
