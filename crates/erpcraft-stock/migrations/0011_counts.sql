-- What a physical count needs of the stock ledger.
--
-- A count in progress holds the levels it counts: the ledger asks the
-- company's policy (stock.count_movement) before anything moves on a level
-- that is held. The count's own book quantities are read from the ledger
-- by the order the entries were recorded in (stock_entry.seq), so they are
-- exact at the count's cut-off whatever moves after it.
--
-- ABC classes of items, per company: what a cycle count is narrowed by
-- and how often each class is counted.

CREATE TABLE stock_count_hold (
    item_id uuid NOT NULL REFERENCES item (id),
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    company_id uuid NOT NULL REFERENCES company (id),
    PRIMARY KEY (item_id, warehouse_id, document_id)
);
CREATE INDEX stock_count_hold_document ON stock_count_hold (document_id);

CREATE TABLE item_abc (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    item_id uuid NOT NULL REFERENCES item (id),
    class text NOT NULL CHECK (class IN ('A', 'B', 'C')),
    -- What the items were ranked by: value on hand, or value issued.
    basis text NOT NULL CHECK (basis IN ('value', 'movement')),
    -- The item's value by that basis, and its share of the running total.
    amount numeric(20, 4) NOT NULL DEFAULT 0,
    cumulative_share numeric(9, 6) NOT NULL DEFAULT 0,
    computed_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, item_id)
);
CREATE INDEX item_abc_class ON item_abc (company_id, class);
