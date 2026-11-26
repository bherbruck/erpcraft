-- Time-phased planning: how many days a bill takes to make (from the
-- order's start to its product being in stock), and when a production
-- order is due. A production order without a due date is due its date
-- plus its bill's lead time.
ALTER TABLE bom ADD COLUMN lead_time_days int NOT NULL DEFAULT 0
    CHECK (lead_time_days BETWEEN 0 AND 3650);
ALTER TABLE production_order ADD COLUMN due_date date;
