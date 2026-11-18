-- Line discounts, charges, lines that are not stock, invoices without an
-- order, and landed cost.
--
-- A line's net amount is its quantity times its unit price, rounded, less
-- a percent discount (rounded) and an amount discount. Goods are received
-- at the order line's net price; an invoice line keeps the net amount it
-- posted, and credit notes are capped by it.
--
-- An order line is an item to receive into a warehouse, or a line that is
-- not stock (a service, rent, a machine to capitalise): a charge or an
-- account, with a description. An invoice line follows an order line, or
-- is a charge or an account line of its own. An invoice without an order
-- names its vendor and currency itself.
--
-- A landed-cost charge line (freight, insurance on goods bought) is
-- allocated to the goods of receipts, by their value or their quantity:
-- the receipts it names, or else every receipt of the invoice's order.
ALTER TABLE purchase_order_line
    ALTER COLUMN item_id DROP NOT NULL,
    ALTER COLUMN warehouse_id DROP NOT NULL,
    ADD COLUMN charge_id uuid REFERENCES charge (id),
    ADD COLUMN account_id uuid REFERENCES account (id),
    ADD COLUMN description text,
    ADD COLUMN discount_percent numeric(9, 6)
        CHECK (discount_percent > 0 AND discount_percent <= 100),
    ADD COLUMN discount_amount numeric(20, 4) CHECK (discount_amount > 0),
    ADD CONSTRAINT purchase_order_line_kind CHECK (
        (item_id IS NOT NULL AND warehouse_id IS NOT NULL
            AND charge_id IS NULL AND account_id IS NULL)
        OR (item_id IS NULL AND warehouse_id IS NULL
            AND num_nonnulls(charge_id, account_id) = 1));

ALTER TABLE vendor_invoice_line
    ALTER COLUMN order_line_id DROP NOT NULL,
    ADD COLUMN charge_id uuid REFERENCES charge (id),
    ADD COLUMN account_id uuid REFERENCES account (id),
    ADD COLUMN description text,
    ADD COLUMN discount_percent numeric(9, 6)
        CHECK (discount_percent > 0 AND discount_percent <= 100),
    ADD COLUMN discount_amount numeric(20, 4) CHECK (discount_amount > 0),
    -- Set when the invoice is posted.
    ADD COLUMN net_amount numeric(20, 4),
    -- For a landed-cost charge: how it is shared out, and over which
    -- receipts (all receipts of the order when none are named).
    ADD COLUMN allocate_by text CHECK (allocate_by IN ('value', 'quantity')),
    ADD COLUMN receipt_ids uuid[],
    ADD CONSTRAINT vendor_invoice_line_kind
        CHECK (num_nonnulls(order_line_id, charge_id, account_id) = 1),
    ADD CONSTRAINT vendor_invoice_line_allocation
        CHECK (charge_id IS NOT NULL OR (allocate_by IS NULL AND receipt_ids IS NULL));

-- Lines posted before: their net amount is the quantity times the price.
ALTER TABLE vendor_invoice_line DISABLE TRIGGER vendor_invoice_line_guard;
UPDATE vendor_invoice_line i
SET net_amount = round(i.qty * i.unit_price, cur.decimals)
FROM vendor_invoice v
JOIN purchase_order o ON o.document_id = v.purchase_order_id
JOIN document d ON d.id = v.document_id
JOIN company co ON co.id = d.company_id
JOIN currency cur ON cur.code = COALESCE(o.currency, co.base_currency)
WHERE v.document_id = i.document_id AND d.status <> 'draft';
ALTER TABLE vendor_invoice_line ENABLE TRIGGER vendor_invoice_line_guard;

ALTER TABLE vendor_credit_note_line
    ADD COLUMN discount_percent numeric(9, 6)
        CHECK (discount_percent > 0 AND discount_percent <= 100),
    ADD COLUMN discount_amount numeric(20, 4) CHECK (discount_amount > 0);

ALTER TABLE vendor_invoice
    ALTER COLUMN purchase_order_id DROP NOT NULL,
    ADD COLUMN vendor_id uuid REFERENCES vendor (id),
    ADD COLUMN currency text REFERENCES currency (code),
    ADD CONSTRAINT vendor_invoice_party
        CHECK ((purchase_order_id IS NULL) <> (vendor_id IS NULL));

ALTER TABLE vendor_credit_note ALTER COLUMN purchase_order_id DROP NOT NULL;

-- What a credit note can take back of an invoice line: what the line
-- posted, net of its discount.
CREATE OR REPLACE FUNCTION vendor_credit_note_line_cap() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    charged numeric;
    credited numeric;
BEGIN
    IF NEW.net_amount IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT COALESCE(i.net_amount, round(i.qty * i.unit_price, cur.decimals)) INTO charged
    FROM vendor_invoice_line i
    JOIN vendor_invoice v ON v.document_id = i.document_id
    LEFT JOIN purchase_order o ON o.document_id = v.purchase_order_id
    JOIN document d ON d.id = i.document_id
    JOIN company co ON co.id = d.company_id
    JOIN currency cur ON cur.code = COALESCE(o.currency, v.currency, co.base_currency)
    WHERE i.id = NEW.invoice_line_id
    FOR UPDATE OF i;
    SELECT COALESCE(sum(c.net_amount), 0) INTO credited
    FROM vendor_credit_note_line c
    JOIN document d ON d.id = c.document_id
    WHERE c.invoice_line_id = NEW.invoice_line_id AND c.id <> NEW.id
      AND c.net_amount IS NOT NULL AND d.status <> 'reversed';
    IF credited + NEW.net_amount > charged THEN
        RAISE EXCEPTION 'a credit note would take back % of an invoice line that charged %, of which % was credited already',
            trim_scale(NEW.net_amount), trim_scale(charged), trim_scale(credited)
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
