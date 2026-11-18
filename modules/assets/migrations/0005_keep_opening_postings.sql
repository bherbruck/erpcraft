-- The opening entry of an asset taken over holds its cost in the ledger as
-- much as the posting a capitalised asset came from does: reversing it
-- would leave the register depreciating what the ledger no longer holds.
-- The same guard refuses it while the asset is in the register.
CREATE OR REPLACE FUNCTION asset_source_reversal_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    held text;
BEGIN
    IF NEW.reverses_id IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT string_agg(a.code || ' (' || a.name || ')', ', ' ORDER BY a.code) INTO held
      FROM asset a JOIN gl_entry g ON g.id = a.source_entry_id OR g.id = a.opening_entry_id
     WHERE g.posting_id = NEW.reverses_id AND a.status <> 'disposed';
    IF held IS NOT NULL THEN
        RAISE EXCEPTION 'the posting holds the cost of fixed asset %, capitalised from it or opened by it: dispose of the asset first', held
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
