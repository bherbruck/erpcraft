-- Reservations: stock on hand set aside for a line of an order (a sales
-- order line, a production order's component) until the goods leave. A
-- reservation never exceeds what was on hand and not reserved for others
-- when it was made; it is cut back as goods leave and removed when the
-- order is done, cancelled or closed. It moves nothing: what may be taken
-- from stock reserved for another order is the company's policy.
CREATE TABLE stock_reservation (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- The order and its line it is set aside for.
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_id uuid NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX stock_reservation_level ON stock_reservation (company_id, item_id, warehouse_id);
CREATE INDEX stock_reservation_document ON stock_reservation (document_id);

CREATE FUNCTION stock_reservation_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM warehouse WHERE id = NEW.warehouse_id
                   AND company_id = NEW.company_id)
       OR NOT EXISTS (SELECT 1 FROM document WHERE id = NEW.document_id
                      AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'a reservation, its warehouse and its order belong to one company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_reservation_check BEFORE INSERT OR UPDATE ON stock_reservation
    FOR EACH ROW EXECUTE FUNCTION stock_reservation_check();
