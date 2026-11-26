-- Time-phased planning (ARCHITECTURE.md, "Time-phased planning (MRP)").
--
-- A reorder rule says how its item and warehouse are planned: by the
-- reorder point, as before, or by MRP, with a safety stock, a minimum
-- order quantity and a lead time of its own (else the vendor's or the
-- bill's).
ALTER TABLE reorder_rule
    ADD COLUMN method text NOT NULL DEFAULT 'reorder' CHECK (method IN ('reorder', 'mrp')),
    ADD COLUMN safety_stock numeric(20, 6) NOT NULL DEFAULT 0 CHECK (safety_stock >= 0),
    ADD COLUMN min_order_qty numeric(20, 6) CHECK (min_order_qty > 0),
    ADD COLUMN lead_time_days int CHECK (lead_time_days BETWEEN 0 AND 3650);

-- What a vendor takes to deliver an item, and the least it sells.
CREATE TABLE item_vendor (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    item_id uuid NOT NULL REFERENCES item (id),
    vendor_id uuid NOT NULL REFERENCES vendor (id),
    lead_time_days int NOT NULL DEFAULT 0 CHECK (lead_time_days BETWEEN 0 AND 3650),
    -- In stock units.
    min_order_qty numeric(20, 6) CHECK (min_order_qty > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, item_id, vendor_id)
);
