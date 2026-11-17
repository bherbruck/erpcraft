-- Purchasing: order, receive, be invoiced.
--
-- The accounting, per company, uses four accounts:
--   receipt:  debit inventory, credit goods received not invoiced (GRNI),
--             at the order price;
--   invoice:  debit GRNI by what the invoiced goods were received for,
--             credit payables by what the vendor charges,
--             and the difference goes to purchase price variance.

-- Vendors are shared by all companies.
CREATE TABLE vendor (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE purchasing_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    inventory_account_id uuid NOT NULL REFERENCES account (id),
    grni_account_id uuid NOT NULL REFERENCES account (id),
    payable_account_id uuid NOT NULL REFERENCES account (id),
    price_variance_account_id uuid NOT NULL REFERENCES account (id),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION purchasing_settings_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM account
        WHERE company_id = NEW.company_id
          AND id IN (NEW.inventory_account_id, NEW.grni_account_id,
                     NEW.payable_account_id, NEW.price_variance_account_id))
       <> (SELECT count(DISTINCT a) FROM unnest(ARRAY[NEW.inventory_account_id, NEW.grni_account_id,
                     NEW.payable_account_id, NEW.price_variance_account_id]) AS a) THEN
        RAISE EXCEPTION 'the purchasing accounts must belong to the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER purchasing_settings_check BEFORE INSERT OR UPDATE ON purchasing_settings
    FOR EACH ROW EXECUTE FUNCTION purchasing_settings_check();

-- ------------------------------------------------------------ purchase order

CREATE TABLE purchase_order (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    vendor_id uuid NOT NULL REFERENCES vendor (id)
);

CREATE TABLE purchase_order_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    item_id uuid NOT NULL REFERENCES item (id),
    -- Where the goods are to arrive.
    warehouse_id uuid NOT NULL REFERENCES warehouse (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- Per unit, in the company's accounting currency.
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    UNIQUE (document_id, line_no)
);

CREATE TRIGGER purchase_order_guard BEFORE INSERT OR UPDATE OR DELETE ON purchase_order
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER purchase_order_line_guard BEFORE INSERT OR UPDATE OR DELETE ON purchase_order_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- How far each order line has got. Kept apart from the line because it
-- changes while the order itself is locked. Changed only by posting or
-- reversing receipts and invoices, under a lock on the order.
CREATE TABLE purchase_order_progress (
    line_id uuid PRIMARY KEY REFERENCES purchase_order_line (id) ON DELETE CASCADE,
    qty_received numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty_received >= 0),
    -- What the received goods came into stock for.
    received_value numeric(20, 4) NOT NULL DEFAULT 0 CHECK (received_value >= 0),
    qty_invoiced numeric(20, 6) NOT NULL DEFAULT 0 CHECK (qty_invoiced >= 0),
    -- How much of received_value invoices have cleared out of GRNI.
    cleared_value numeric(20, 4) NOT NULL DEFAULT 0 CHECK (cleared_value >= 0),
    CONSTRAINT invoiced_within_received CHECK (qty_invoiced <= qty_received),
    CONSTRAINT cleared_within_received CHECK (cleared_value <= received_value),
    -- Nothing received and not invoiced can still carry value, and the
    -- other way round.
    CONSTRAINT nothing_left_behind CHECK (qty_invoiced < qty_received OR cleared_value = received_value)
);

CREATE FUNCTION purchase_order_progress_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.qty_received > (SELECT qty FROM purchase_order_line WHERE id = NEW.line_id) THEN
        RAISE EXCEPTION 'more cannot be received than was ordered'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER purchase_order_progress_check BEFORE INSERT OR UPDATE ON purchase_order_progress
    FOR EACH ROW EXECUTE FUNCTION purchase_order_progress_check();

-- ---------------------------------------------------------- purchase receipt

CREATE TABLE purchase_receipt (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    purchase_order_id uuid NOT NULL REFERENCES document (id)
);

CREATE TABLE purchase_receipt_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    order_line_id uuid NOT NULL REFERENCES purchase_order_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What the goods came into stock for. Set when the receipt is posted.
    value numeric(20, 4),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, order_line_id)
);

CREATE TRIGGER purchase_receipt_guard BEFORE INSERT OR UPDATE OR DELETE ON purchase_receipt
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER purchase_receipt_line_guard BEFORE INSERT OR UPDATE OR DELETE ON purchase_receipt_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- ------------------------------------------------------------ vendor invoice

CREATE TABLE vendor_invoice (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    purchase_order_id uuid NOT NULL REFERENCES document (id),
    -- The vendor's own invoice number.
    vendor_reference text
);

CREATE TABLE vendor_invoice_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    order_line_id uuid NOT NULL REFERENCES purchase_order_line (id),
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    -- What the vendor charges per unit.
    unit_price numeric(20, 6) NOT NULL CHECK (unit_price >= 0),
    -- What this line cleared out of GRNI. Set when the invoice is posted.
    cleared_value numeric(20, 4),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, order_line_id)
);

CREATE TRIGGER vendor_invoice_guard BEFORE INSERT OR UPDATE OR DELETE ON vendor_invoice
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
CREATE TRIGGER vendor_invoice_line_guard BEFORE INSERT OR UPDATE OR DELETE ON vendor_invoice_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
