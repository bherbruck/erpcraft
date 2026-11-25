-- An item is settled no earlier than its own date. A payment dated before
-- the invoice it pays would credit the receivable on a day the item did
-- not yet exist, so the ledger and the open items as of that day would
-- disagree. Money received before the invoice is money on account, applied
-- to the invoice once there is one. A reversal, which calls an item off,
-- is dated no earlier than what it reverses anyway.
CREATE FUNCTION settlement_after_item() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    item_date date;
    settled_on date;
BEGIN
    IF NEW.cancels OR NEW.reverses_id IS NOT NULL THEN
        RETURN NEW;
    END IF;
    SELECT doc_date INTO item_date FROM open_item WHERE id = NEW.open_item_id;
    SELECT posting_date INTO settled_on FROM posting WHERE id = NEW.posting_id;
    IF settled_on < item_date THEN
        RAISE EXCEPTION 'an item dated % cannot be settled by a posting dated %: take the money on account and apply it once the item exists',
            item_date, settled_on USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER settlement_after_item BEFORE INSERT ON settlement
    FOR EACH ROW EXECUTE FUNCTION settlement_after_item();
