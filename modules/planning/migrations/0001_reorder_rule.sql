-- Reorder rules: for an item in a warehouse, the least a company wants to
-- have once every open order is done (min_qty), how far to fill up when
-- it falls below (max_qty, else min_qty), in what multiples it is bought
-- or made (a pack size), and whom it is usually bought from at what price.
-- Planning suggests what to order or make from them; it orders nothing by
-- itself.
CREATE TABLE reorder_rule (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    min_qty numeric(20, 6) NOT NULL DEFAULT 0 CHECK (min_qty >= 0),
    max_qty numeric(20, 6) CHECK (max_qty >= min_qty),
    multiple numeric(20, 6) CHECK (multiple > 0),
    vendor_id uuid REFERENCES vendor (id),
    -- In the company's accounting currency, per unit.
    unit_price numeric(20, 6) CHECK (unit_price >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (item_id, warehouse_id)
);

CREATE FUNCTION reorder_rule_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM warehouse WHERE id = NEW.warehouse_id
                   AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'the warehouse of a reorder rule must belong to its company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER reorder_rule_check BEFORE INSERT OR UPDATE ON reorder_rule
    FOR EACH ROW EXECUTE FUNCTION reorder_rule_check();
