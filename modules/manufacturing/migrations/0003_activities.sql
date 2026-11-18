-- Labor and overhead: what making a product costs beyond its components.
-- An activity (assembly labor, machine hours, factory overhead) is shared
-- by the companies, as bills are; each company charges it at a rate of
-- its own, in its accounting currency, per unit of the activity (an hour).
-- A bill says how much of each activity one run takes.
--
-- Releasing an order copies the activities of its bill, scaled to the
-- order, with the company's rate at that moment. Completing it brings the
-- product in at what its components were worth plus each activity's
-- quantity times its rate, and credits that to the account for
-- `absorbed_cost` (by activity, else the company's): the cost absorbed
-- into stock, against which the actual wages and overhead are booked.
CREATE TABLE cost_activity (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    kind text NOT NULL DEFAULT 'labor' CHECK (kind IN ('labor', 'machine', 'overhead')),
    -- What the quantity counts: h, min, run.
    unit text NOT NULL DEFAULT 'h' CHECK (unit <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cost_activity_rate (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    activity_id uuid NOT NULL REFERENCES cost_activity (id) ON DELETE CASCADE,
    -- Per unit of the activity, in the company's accounting currency.
    rate numeric(20, 6) NOT NULL CHECK (rate >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, activity_id)
);

CREATE TABLE bom_activity (
    id uuid PRIMARY KEY,
    bom_id uuid NOT NULL REFERENCES bom (id) ON DELETE CASCADE,
    activity_id uuid NOT NULL REFERENCES cost_activity (id),
    -- Units of the activity one run of the bill takes.
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bom_id, activity_id)
);

CREATE FUNCTION bom_activity_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.bom_id <> OLD.bom_id THEN
        RAISE EXCEPTION 'an activity cannot move to another bill of materials'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER bom_activity_check BEFORE INSERT OR UPDATE ON bom_activity
    FOR EACH ROW EXECUTE FUNCTION bom_activity_check();

-- What a released order will absorb: the bill's activities scaled to the
-- order, at the company's rate when it was released.
CREATE TABLE production_order_activity (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    activity_id uuid NOT NULL REFERENCES cost_activity (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    rate numeric(20, 6) NOT NULL CHECK (rate >= 0),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, activity_id)
);
CREATE TRIGGER production_order_activity_guard
    BEFORE INSERT OR UPDATE OR DELETE ON production_order_activity
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
