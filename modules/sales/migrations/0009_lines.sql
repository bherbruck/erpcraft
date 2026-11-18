-- Line discounts, charges, lines that are not stock, and invoices without
-- an order.
--
-- A line's net amount is its quantity times its unit price, rounded, less
-- a percent discount (rounded) and an amount discount. An invoice line
-- keeps the net amount it posted; credit notes are capped by it.
--
-- An order line is an item to deliver from a warehouse, or a line that is
-- not stock: a charge (freight, a service) or an account, with a
-- description. An invoice line follows an order line, or is a charge or an
-- account line of its own. An invoice without an order names its customer
-- and currency itself.
ALTER TABLE sales_order_line
    ALTER COLUMN item_id DROP NOT NULL,
    ALTER COLUMN warehouse_id DROP NOT NULL,
    ADD COLUMN charge_id uuid REFERENCES charge (id),
    ADD COLUMN account_id uuid REFERENCES account (id),
    ADD COLUMN description text,
    ADD COLUMN discount_percent numeric(9, 6)
        CHECK (discount_percent > 0 AND discount_percent <= 100),
    ADD COLUMN discount_amount numeric(20, 4) CHECK (discount_amount > 0),
    ADD CONSTRAINT sales_order_line_kind CHECK (
        (item_id IS NOT NULL AND warehouse_id IS NOT NULL
            AND charge_id IS NULL AND account_id IS NULL)
        OR (item_id IS NULL AND warehouse_id IS NULL
            AND num_nonnulls(charge_id, account_id) = 1));

ALTER TABLE sales_invoice_line
    ALTER COLUMN order_line_id DROP NOT NULL,
    ADD COLUMN charge_id uuid REFERENCES charge (id),
    ADD COLUMN account_id uuid REFERENCES account (id),
    ADD COLUMN description text,
    ADD COLUMN discount_percent numeric(9, 6)
        CHECK (discount_percent > 0 AND discount_percent <= 100),
    ADD COLUMN discount_amount numeric(20, 4) CHECK (discount_amount > 0),
    -- Set when the invoice is posted.
    ADD COLUMN net_amount numeric(20, 4),
    ADD CONSTRAINT sales_invoice_line_kind
        CHECK (num_nonnulls(order_line_id, charge_id, account_id) = 1);

-- Lines posted before: their net amount is the quantity times the price.
ALTER TABLE sales_invoice_line DISABLE TRIGGER sales_invoice_line_guard;
UPDATE sales_invoice_line i
SET net_amount = round(i.qty * i.unit_price, cur.decimals)
FROM sales_invoice s
JOIN sales_order o ON o.document_id = s.sales_order_id
JOIN document d ON d.id = s.document_id
JOIN company co ON co.id = d.company_id
JOIN currency cur ON cur.code = COALESCE(o.currency, co.base_currency)
WHERE s.document_id = i.document_id AND d.status <> 'draft';
ALTER TABLE sales_invoice_line ENABLE TRIGGER sales_invoice_line_guard;

ALTER TABLE sales_credit_note_line
    ADD COLUMN discount_percent numeric(9, 6)
        CHECK (discount_percent > 0 AND discount_percent <= 100),
    ADD COLUMN discount_amount numeric(20, 4) CHECK (discount_amount > 0);

ALTER TABLE sales_invoice
    ALTER COLUMN sales_order_id DROP NOT NULL,
    ADD COLUMN customer_id uuid REFERENCES customer (id),
    ADD COLUMN currency text REFERENCES currency (code),
    ADD CONSTRAINT sales_invoice_party CHECK ((sales_order_id IS NULL) <> (customer_id IS NULL));

ALTER TABLE sales_credit_note ALTER COLUMN sales_order_id DROP NOT NULL;

-- What a credit note can take back of an invoice line: what the line
-- posted, net of its discount.
CREATE OR REPLACE FUNCTION sales_credit_note_line_cap() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    charged numeric;
    credited numeric;
BEGIN
    IF NEW.net_amount IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT COALESCE(i.net_amount, round(i.qty * i.unit_price, cur.decimals)) INTO charged
    FROM sales_invoice_line i
    JOIN sales_invoice s ON s.document_id = i.document_id
    LEFT JOIN sales_order o ON o.document_id = s.sales_order_id
    JOIN document d ON d.id = i.document_id
    JOIN company co ON co.id = d.company_id
    JOIN currency cur ON cur.code = COALESCE(o.currency, s.currency, co.base_currency)
    WHERE i.id = NEW.invoice_line_id
    FOR UPDATE OF i;
    SELECT COALESCE(sum(c.net_amount), 0) INTO credited
    FROM sales_credit_note_line c
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
