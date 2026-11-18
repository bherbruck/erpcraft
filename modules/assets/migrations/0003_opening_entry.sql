-- The opening entry of an asset taken over from another system: the ledger
-- line that brought its cost into the books. Several assets taken over
-- together may share one line (one opening line per class, say), so it is
-- not unique as a capitalised asset's source entry is. A line that opened
-- assets taken over is not a line to capitalise another asset from.
ALTER TABLE asset
    ADD COLUMN opening_entry_id uuid REFERENCES gl_entry (id),
    ADD CONSTRAINT asset_opening_or_source
        CHECK (opening_entry_id IS NULL OR source_entry_id IS NULL);
CREATE INDEX asset_opening_entry ON asset (opening_entry_id) WHERE opening_entry_id IS NOT NULL;
