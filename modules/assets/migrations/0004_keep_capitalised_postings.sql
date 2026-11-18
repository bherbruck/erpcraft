-- The posting an asset was capitalised from holds the asset's cost in the
-- ledger: while the asset is in the register (not disposed of), that
-- posting is not reversed, or the register would depreciate what the
-- ledger no longer holds. The ledger knows nothing of assets, so the guard
-- is the assets module's, on the reversal's posting.
CREATE FUNCTION asset_source_reversal_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    held text;
BEGIN
    IF NEW.reverses_id IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT string_agg(a.code || ' (' || a.name || ')', ', ' ORDER BY a.code) INTO held
      FROM asset a JOIN gl_entry g ON g.id = a.source_entry_id
     WHERE g.posting_id = NEW.reverses_id AND a.status <> 'disposed';
    IF held IS NOT NULL THEN
        RAISE EXCEPTION 'the posting holds the cost of fixed asset %, capitalised from it: dispose of the asset first', held
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER asset_source_reversal_check BEFORE INSERT ON posting
    FOR EACH ROW EXECUTE FUNCTION asset_source_reversal_check();
