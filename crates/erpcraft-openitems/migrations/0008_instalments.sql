-- Payment terms that make an invoice due in parts: 30 % on the invoice's
-- date, 30 % within 30 days, the rest at the terms' net days. Each row is
-- one part before the last; the last part is what is left, due when the
-- terms say the invoice is due.
CREATE TABLE payment_terms_instalment (
    id uuid PRIMARY KEY,
    terms_id uuid NOT NULL REFERENCES payment_terms (id) ON DELETE CASCADE,
    percent numeric(9, 6) NOT NULL CHECK (percent > 0 AND percent < 100),
    -- Counted as the terms' net days are.
    days integer NOT NULL CHECK (days >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (terms_id, days)
);

-- The parts come before the last and leave something for it.
CREATE FUNCTION payment_terms_instalment_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    net integer;
    total numeric;
BEGIN
    SELECT net_days INTO net FROM payment_terms WHERE id = NEW.terms_id;
    IF NEW.days >= net THEN
        RAISE EXCEPTION 'a part is due before the terms'' net days (%), when the rest is due', net
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT COALESCE(sum(percent), 0) + NEW.percent INTO total FROM payment_terms_instalment
     WHERE terms_id = NEW.terms_id AND id <> NEW.id;
    IF total >= 100 THEN
        RAISE EXCEPTION 'the parts come to % %%; they must leave something for the last, due at the net days',
            trim_scale(total)
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER payment_terms_instalment_check BEFORE INSERT OR UPDATE ON payment_terms_instalment
    FOR EACH ROW EXECUTE FUNCTION payment_terms_instalment_check();

CREATE FUNCTION payment_terms_net_after_instalments() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM payment_terms_instalment
               WHERE terms_id = NEW.id AND days >= NEW.net_days) THEN
        RAISE EXCEPTION 'the net days must come after every part the terms are due in'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER payment_terms_net_after_instalments AFTER UPDATE OF net_days ON payment_terms
    FOR EACH ROW EXECUTE FUNCTION payment_terms_net_after_instalments();

-- The parts an item is due in, in the item's currency, earliest first. An
-- item due at once has none. They add up to the item.
CREATE TABLE open_item_instalment (
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    seq integer NOT NULL CHECK (seq >= 1),
    due_date date NOT NULL,
    txn_amount numeric(20, 4) NOT NULL CHECK (txn_amount > 0),
    PRIMARY KEY (open_item_id, seq)
);
CREATE TRIGGER open_item_instalment_immutable BEFORE UPDATE OR DELETE ON open_item_instalment
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER open_item_instalment_no_truncate BEFORE TRUNCATE ON open_item_instalment
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- Checked at commit, once all parts are in: they add up to the item.
CREATE FUNCTION open_item_instalment_total() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    item open_item%ROWTYPE;
    total numeric;
BEGIN
    SELECT * INTO item FROM open_item WHERE id = NEW.open_item_id;
    SELECT sum(txn_amount) INTO total FROM open_item_instalment
     WHERE open_item_id = NEW.open_item_id;
    IF item.txn_amount <= 0 OR total <> item.txn_amount THEN
        RAISE EXCEPTION 'the parts of open item % come to %, not %',
            item.id, trim_scale(total), trim_scale(item.txn_amount)
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER open_item_instalment_total AFTER INSERT ON open_item_instalment
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION open_item_instalment_total();

-- A settlement may name the part it pays. One that names none pays the
-- earliest part still open first.
ALTER TABLE settlement ADD COLUMN instalment integer;
ALTER TABLE settlement ADD CONSTRAINT settlement_instalment_fk
    FOREIGN KEY (open_item_id, instalment) REFERENCES open_item_instalment (open_item_id, seq);

CREATE FUNCTION settlement_instalment() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    part open_item_instalment%ROWTYPE;
    paid numeric;
BEGIN
    IF NEW.reverses_id IS NOT NULL THEN
        NEW.instalment := (SELECT instalment FROM settlement WHERE id = NEW.reverses_id);
        RETURN NEW;
    END IF;
    IF NEW.instalment IS NULL THEN
        RETURN NEW;
    END IF;
    IF NEW.cancels THEN
        RAISE EXCEPTION 'calling off an item settles all of it, not a part'
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT * INTO part FROM open_item_instalment
     WHERE open_item_id = NEW.open_item_id AND seq = NEW.instalment;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'the item has no part %', NEW.instalment
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT COALESCE(sum(txn_amount), 0) INTO paid FROM settlement
     WHERE open_item_id = NEW.open_item_id AND instalment = NEW.instalment;
    IF paid + NEW.txn_amount > part.txn_amount THEN
        RAISE EXCEPTION 'part % of the item is %, of which % was paid already; not %',
            part.seq, trim_scale(part.txn_amount), trim_scale(paid), trim_scale(NEW.txn_amount)
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
-- Named to run after settlement_before_insert, which sets the amounts, and
-- before settlement_z_discount.
CREATE TRIGGER settlement_y_instalment BEFORE INSERT ON settlement
    FOR EACH ROW EXECUTE FUNCTION settlement_instalment();

-- Each part of an item as it stands on `as_of` (all settlements when
-- null), in the item's currency: what was paid for it by name, and then,
-- earliest part first, what was paid for the item without naming a part.
CREATE FUNCTION open_item_instalments(item uuid, as_of date)
RETURNS TABLE (seq integer, due_date date, txn_amount numeric, txn_settled numeric)
LANGUAGE plpgsql STABLE AS $$
DECLARE
    unnamed numeric;
    part record;
    take numeric;
BEGIN
    SELECT COALESCE(sum(s.txn_amount) FILTER (WHERE s.instalment IS NULL), 0) INTO unnamed
      FROM settlement s JOIN posting p ON p.id = s.posting_id
     WHERE s.open_item_id = item AND (as_of IS NULL OR p.posting_date <= as_of);
    FOR part IN
        SELECT i.seq, i.due_date, i.txn_amount,
               COALESCE((SELECT sum(s.txn_amount) FROM settlement s
                         JOIN posting p ON p.id = s.posting_id
                         WHERE s.open_item_id = item AND s.instalment = i.seq
                           AND (as_of IS NULL OR p.posting_date <= as_of)), 0) AS named
          FROM open_item_instalment i WHERE i.open_item_id = item ORDER BY i.seq
    LOOP
        take := LEAST(GREATEST(unnamed, 0), part.txn_amount - part.named);
        unnamed := unnamed - take;
        seq := part.seq;
        due_date := part.due_date;
        txn_amount := part.txn_amount;
        txn_settled := part.named + take;
        RETURN NEXT;
    END LOOP;
END $$;
