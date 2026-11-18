-- Work centers and routings. A work center is where an operation is done:
-- an assembly line, a machine. It belongs to a company (and, where the
-- company has several, to a site), charges its time at a cost activity's
-- rate, and can work so many hours a day.
--
-- A bill's routing is its operations in order, each at a work center by
-- the center's code (bills are shared by companies; a company that makes
-- the product has a work center of that code), with a setup time per
-- order and a run time per unit made. A released order copies them, in
-- hours for its quantity, at the rate then in force; completing it
-- absorbs that standard time into the product. Actual time is recorded
-- per operation, and the difference from standard is posted to the
-- account for `production_variance`.
CREATE TABLE work_center (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- The site it stands at, where the company has sites.
    site_id uuid,
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- What an hour here costs: the activity's rate for the company.
    activity_id uuid NOT NULL REFERENCES cost_activity (id),
    hours_per_day numeric(10, 2) NOT NULL DEFAULT 8 CHECK (hours_per_day > 0 AND hours_per_day <= 24),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code)
);

CREATE TABLE bom_operation (
    id uuid PRIMARY KEY,
    bom_id uuid NOT NULL REFERENCES bom (id) ON DELETE CASCADE,
    seq integer NOT NULL CHECK (seq > 0),
    work_center_code text NOT NULL CHECK (work_center_code <> ''),
    description text,
    -- Hours once per order, and hours per unit of the product made.
    setup_hours numeric(20, 6) NOT NULL DEFAULT 0 CHECK (setup_hours >= 0),
    run_hours numeric(20, 6) NOT NULL DEFAULT 0 CHECK (run_hours >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bom_id, seq),
    CHECK (setup_hours + run_hours > 0)
);

-- A released order's operations: its standard hours and their rate.
CREATE TABLE production_order_operation (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    seq integer NOT NULL,
    work_center_id uuid NOT NULL REFERENCES work_center (id),
    activity_id uuid NOT NULL REFERENCES cost_activity (id),
    description text,
    -- Setup plus run time for the order's quantity.
    hours numeric(20, 6) NOT NULL CHECK (hours > 0),
    rate numeric(20, 6) NOT NULL CHECK (rate >= 0),
    UNIQUE (document_id, line_no)
);
CREATE TRIGGER production_order_operation_guard
    BEFORE INSERT OR UPDATE OR DELETE ON production_order_operation
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- Time actually spent on an operation, recorded while the order is
-- released. Kept beside the order, which is locked. Never changed: a
-- mistake is corrected by a negative entry.
CREATE TABLE production_operation_time (
    id uuid PRIMARY KEY,
    operation_id uuid NOT NULL REFERENCES production_order_operation (id) ON DELETE CASCADE,
    hours numeric(20, 6) NOT NULL CHECK (hours <> 0),
    worked_on date NOT NULL,
    note text,
    recorded_by uuid NOT NULL REFERENCES app_user (id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX production_operation_time_operation ON production_operation_time (operation_id);
CREATE TRIGGER production_operation_time_immutable BEFORE UPDATE OR DELETE ON production_operation_time
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
