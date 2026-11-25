-- Payment runs: what is due to vendors, proposed, approved and paid in
-- one batch through ordinary payments.

CREATE TABLE payment_hold (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- A vendor, or one open item.
    vendor_id uuid REFERENCES vendor (id),
    open_item_id uuid REFERENCES open_item (id),
    reason text NOT NULL CHECK (reason <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((vendor_id IS NULL) <> (open_item_id IS NULL))
);
CREATE INDEX payment_hold_company ON payment_hold (company_id) WHERE active;

CREATE TABLE payment_run (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    bank_account_id uuid NOT NULL REFERENCES account (id),
    -- Items due by this day are proposed.
    due_by date NOT NULL,
    currency text NOT NULL REFERENCES currency (code),
    take_discounts boolean NOT NULL DEFAULT true
);

CREATE TABLE payment_run_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    vendor_id uuid NOT NULL REFERENCES vendor (id),
    due_date date,
    discount_date date,
    -- In the run's currency: what is paid, and the discount kept back.
    amount numeric(20, 4) NOT NULL,
    discount numeric(20, 4) NOT NULL DEFAULT 0 CHECK (discount >= 0),
    -- Kept on the run but not paid.
    hold boolean NOT NULL DEFAULT false,
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, open_item_id)
);
CREATE INDEX payment_run_line_item ON payment_run_line (open_item_id);

-- The payment a posted run made to each vendor.
CREATE TABLE payment_run_payment (
    document_id uuid NOT NULL REFERENCES document (id),
    vendor_id uuid NOT NULL REFERENCES vendor (id),
    payment_id uuid NOT NULL UNIQUE REFERENCES document (id),
    PRIMARY KEY (document_id, vendor_id)
);

CREATE TRIGGER payment_run_guard BEFORE INSERT OR UPDATE OR DELETE ON payment_run
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER payment_run_line_guard BEFORE INSERT OR UPDATE OR DELETE ON payment_run_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
