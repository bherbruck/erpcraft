-- Sales: order, deliver, invoice.
--
-- The accounting, per company, uses four accounts:
--   delivery: debit cost of goods sold, credit inventory, at the moving
--             average value the goods left stock for;
--   invoice:  debit receivables, credit revenue, by what the customer is
--             charged.

-- Customers are shared by all companies.
CREATE TABLE customer (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sales_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    receivable_account_id uuid NOT NULL REFERENCES account (id),
    revenue_account_id uuid NOT NULL REFERENCES account (id),
    cogs_account_id uuid NOT NULL REFERENCES account (id),
    inventory_account_id uuid NOT NULL REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION sales_settings_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM account
        WHERE company_id = NEW.company_id
          AND id IN (NEW.receivable_account_id, NEW.revenue_account_id,
                     NEW.cogs_account_id, NEW.inventory_account_id))
       <> (SELECT count(DISTINCT a) FROM unnest(ARRAY[NEW.receivable_account_id, NEW.revenue_account_id,
                     NEW.cogs_account_id, NEW.inventory_account_id]) AS a) THEN
        RAISE EXCEPTION 'the sales accounts must belong to the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sales_settings_check BEFORE INSERT OR UPDATE ON sales_settings
    FOR EACH ROW EXECUTE FUNCTION sales_settings_check();

-- --------------------------------------------------------------- sales order

CREATE TABLE sales_order (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    customer_id uuid NOT NULL REFERENCES customer (id)
);

CREATE TABLE sales_order_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    -- Where the goods are to leave from.
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- Per unit, in the company's accounting currency.
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER sales_order_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_order
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_order_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_order_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- How far each order line has got. Kept apart from the line because it
-- changes while the order itself is locked. Changed only by posting or
-- reversing deliveries and invoices, under a lock on the order.
CREATE TABLE sales_order_progress (
    line_id uuid PRIMARY KEY REFERENCES sales_order_line (id) ON DELETE CASCADE,
    qty_delivered numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty_delivered >= 0),
    qty_invoiced numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty_invoiced >= 0),
    CONSTRAINT invoiced_within_delivered CHECK (qty_invoiced <= qty_delivered)
);

CREATE FUNCTION sales_order_progress_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.qty_delivered > (SELECT qty FROM sales_order_line WHERE id = NEW.line_id) THEN
        RAISE EXCEPTION 'more cannot be delivered than was ordered'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER sales_order_progress_check BEFORE INSERT OR UPDATE ON sales_order_progress
    FOR EACH ROW EXECUTE FUNCTION sales_order_progress_check();

-- ------------------------------------------------------------ sales delivery

CREATE TABLE sales_delivery (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    sales_order_id uuid NOT NULL REFERENCES document (id)
);

CREATE TABLE sales_delivery_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    order_line_id uuid NOT NULL REFERENCES sales_order_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What the goods left stock for: their cost. Set when the delivery is
    -- posted.
    value numeric(20, 4) CHECK (value >= 0),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, order_line_id)
);

CREATE TRIGGER sales_delivery_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_delivery
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_delivery_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_delivery_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- ------------------------------------------------------------- sales invoice

CREATE TABLE sales_invoice (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    sales_order_id uuid NOT NULL REFERENCES document (id)
);

CREATE TABLE sales_invoice_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    order_line_id uuid NOT NULL REFERENCES sales_order_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What the customer is charged per unit.
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, order_line_id)
);

CREATE TRIGGER sales_invoice_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_invoice
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER sales_invoice_line_guard BEFORE INSERT OR UPDATE OR DELETE ON sales_invoice_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
