-- The dimension values a journal entry line states (ids of the general
-- ledger's dimension_value), at most one per dimension. The company's
-- other dimensions take their defaults when the entry is posted.
ALTER TABLE journal_entry_line ADD COLUMN dimension_value_ids uuid[] NOT NULL DEFAULT '{}';
