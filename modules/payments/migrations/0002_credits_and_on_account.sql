-- A payment settles debts and credits of one party together: an
-- allocation to a credit (a credit note, money paid on account) is below
-- zero. A payment whose allocations come to nothing moves no money and
-- names no bank account: it applies a credit to a debt. Money paid beyond
-- the allocations stays with the party as a credit of its own (on account).

ALTER TABLE payment ALTER COLUMN bank_account_id DROP NOT NULL;
ALTER TABLE payment
    -- Money beyond the allocations, kept as a credit of the party.
    ADD COLUMN on_account numeric(20, 4) NOT NULL DEFAULT 0 CHECK (on_account >= 0),
    -- Whose credit, and on which account, when no allocation says so.
    ADD COLUMN party_type text,
    ADD COLUMN party_id uuid,
    ADD COLUMN account_id uuid REFERENCES account (id);

ALTER TABLE payment_allocation DROP CONSTRAINT payment_allocation_amount_check;
ALTER TABLE payment_allocation ADD CONSTRAINT payment_allocation_amount_check CHECK (amount <> 0);
