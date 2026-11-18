-- An entry's amount on one line, debit positive and credit negative, so that
-- a reference to an entry can show it.
ALTER TABLE gl_entry ADD COLUMN amount numeric(20, 4) GENERATED ALWAYS AS (debit - credit) STORED;
