-- Physical inventory and cycle counts.
--
-- A count covers a warehouse and the locations inside it, narrowed for a
-- cycle count to an ABC class and to items. Starting it takes the stock
-- ledger's order number (the cut-off) and writes a line per item,
-- warehouse and lot on hand, with the book quantity at the cut-off. Lines
-- take what was counted, and a recount where the company's tolerance asks
-- for one. Posting it adjusts the differences through stock adjustments.

CREATE TABLE stock_count (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    abc_class text CHECK (abc_class IN ('A', 'B', 'C')),
    item_ids uuid[] NOT NULL DEFAULT '{}',
    -- Hide the book quantity from the count sheet until posted.
    blind boolean NOT NULL DEFAULT false,
    -- The stock ledger's order number when the count started.
    cut_off_seq bigint,
    started_at timestamptz
);

CREATE TABLE stock_count_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    -- For a tracked item: the lot or serial number counted.
    lot text CHECK (lot <> ''),
    -- What the book held at the cut-off.
    book_qty numeric(20, 6) NOT NULL,
    counted_qty numeric(20, 6) CHECK (counted_qty >= 0),
    -- Set by the tolerance policy: the line must be counted again.
    recount boolean NOT NULL DEFAULT false,
    recount_qty numeric(20, 6) CHECK (recount_qty >= 0),
    UNIQUE (document_id, line_no),
    UNIQUE NULLS NOT DISTINCT (document_id, item_id, warehouse_id, lot)
);
CREATE INDEX stock_count_line_item ON stock_count_line (item_id, warehouse_id);

CREATE TRIGGER stock_count_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_count
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER stock_count_line_guard BEFORE INSERT OR UPDATE OR DELETE ON stock_count_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- How often items of each ABC class are counted, per company.
CREATE TABLE cycle_count_rule (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    class text NOT NULL CHECK (class IN ('A', 'B', 'C')),
    every_days integer NOT NULL CHECK (every_days > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, class)
);
