-- Cost versions: the standard costs of a company's items, rolled up over
-- bills of materials and routings, and activated on a date, which writes
-- the standards (item_standard) and revalues the stock on hand.

CREATE TABLE cost_version_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    unit_cost numeric(20, 6) NOT NULL CHECK (unit_cost >= 0),
    material_cost numeric(20, 6) NOT NULL DEFAULT 0 CHECK (material_cost >= 0),
    labour_cost numeric(20, 6) NOT NULL DEFAULT 0 CHECK (labour_cost >= 0),
    overhead_cost numeric(20, 6) NOT NULL DEFAULT 0 CHECK (overhead_cost >= 0),
    -- Worked out by cost_version.rollup from the item's bill and routing.
    rolled_up boolean NOT NULL DEFAULT false,
    -- Move the item from moving average to standard when activated.
    switch_to_standard boolean NOT NULL DEFAULT false,
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, item_id)
);
CREATE TRIGGER cost_version_line_guard BEFORE INSERT OR UPDATE OR DELETE ON cost_version_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
