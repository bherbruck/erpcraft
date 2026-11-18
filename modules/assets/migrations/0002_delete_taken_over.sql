-- An asset taken over from another system posts nothing: it may be deleted
-- until something refers to it. One capitalised from a ledger line, or
-- depreciated or disposed of, is in the books: it is disposed of, not
-- deleted.
CREATE OR REPLACE FUNCTION asset_delete_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.source_entry_id IS NOT NULL THEN
        RAISE EXCEPTION 'an asset capitalised from a ledger line cannot be deleted; dispose of it instead'
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (SELECT 1 FROM asset_depreciation WHERE asset_id = OLD.id)
       OR EXISTS (SELECT 1 FROM depreciation_run_line WHERE asset_id = OLD.id) THEN
        RAISE EXCEPTION 'the asset has been depreciated, even if that was reversed; dispose of it instead'
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (SELECT 1 FROM asset_disposal WHERE asset_id = OLD.id) THEN
        RAISE EXCEPTION 'a disposal names the asset; delete the disposal first, or dispose of the asset'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN OLD;
END $$;
