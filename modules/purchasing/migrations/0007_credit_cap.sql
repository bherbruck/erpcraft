-- A vendor credit note never takes back more than the invoice line it
-- credits charged. Each credit note line keeps its net amount once posted
-- (quantity times price, rounded as the posting rounds it), and the
-- credits of one invoice line, other than those reversed, never add up to
-- more than that line charged. Tax has the same rule in the tax ledger.

ALTER TABLE vendor_credit_note_line ADD COLUMN net_amount numeric(20, 4)
    CHECK (net_amount >= 0);

-- What the lines of credit notes already posted took back.
ALTER TABLE vendor_credit_note_line DISABLE TRIGGER vendor_credit_note_line_guard;
UPDATE vendor_credit_note_line c
SET net_amount = round(c.qty * c.unit_price, cur.decimals)
FROM document d, vendor_credit_note n, purchase_order o, company co, currency cur
WHERE d.id = c.document_id AND d.status <> 'draft'
  AND n.document_id = c.document_id
  AND o.document_id = n.purchase_order_id
  AND co.id = d.company_id
  AND cur.code = COALESCE(o.currency, co.base_currency);
ALTER TABLE vendor_credit_note_line ENABLE TRIGGER vendor_credit_note_line_guard;

CREATE FUNCTION vendor_credit_note_line_cap() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    charged numeric;
    credited numeric;
BEGIN
    IF NEW.net_amount IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT round(i.qty * i.unit_price, cur.decimals) INTO charged
    FROM vendor_invoice_line i
    JOIN vendor_invoice v ON v.document_id = i.document_id
    JOIN purchase_order o ON o.document_id = v.purchase_order_id
    JOIN document d ON d.id = i.document_id
    JOIN company co ON co.id = d.company_id
    JOIN currency cur ON cur.code = COALESCE(o.currency, co.base_currency)
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
CREATE TRIGGER vendor_credit_note_line_cap BEFORE INSERT OR UPDATE OF net_amount
    ON vendor_credit_note_line FOR EACH ROW EXECUTE FUNCTION vendor_credit_note_line_cap();
