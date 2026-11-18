-- Two people adding parts to the same payment terms at once each saw the
-- other's part missing, and together could take the parts to 100 % or
-- more. The terms row is locked first, so the second waits and counts the
-- first's part.
CREATE OR REPLACE FUNCTION payment_terms_instalment_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    net integer;
    total numeric;
BEGIN
    SELECT net_days INTO net FROM payment_terms WHERE id = NEW.terms_id FOR UPDATE;
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

-- A settlement that names a part pays no more than is open of it, counting
-- what was paid without naming a part, as open_item_instalments shares it
-- out (earliest part first). Before, only what was paid by name counted, so
-- a payment naming part 1 posted beside an unnamed one could both pass and
-- the named one end up paying part 2. The item is locked first, as every
-- writer of its settlements does, so what is open is read after any
-- payment that held it.
CREATE OR REPLACE FUNCTION settlement_instalment() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    part record;
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
    PERFORM 1 FROM open_item WHERE id = NEW.open_item_id FOR UPDATE;
    SELECT * INTO part FROM open_item_instalments(NEW.open_item_id, NULL) x
     WHERE x.seq = NEW.instalment;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'the item has no part %', NEW.instalment
            USING ERRCODE = 'check_violation';
    END IF;
    IF part.txn_settled + NEW.txn_amount > part.txn_amount THEN
        RAISE EXCEPTION 'part % of the item is %, of which % is paid already; not %',
            part.seq, trim_scale(part.txn_amount), trim_scale(part.txn_settled),
            trim_scale(NEW.txn_amount)
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
