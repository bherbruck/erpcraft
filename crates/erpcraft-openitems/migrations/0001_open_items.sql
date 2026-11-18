-- Open items: amounts owed to a party or by a party, each created by one
-- posting (an invoice) and settled by others (payments). This is the
-- detail behind the payables and receivables accounts of the general
-- ledger. As with the ledgers, the rules are enforced here.

CREATE TABLE open_item (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- The posting that created the debt.
    posting_id uuid NOT NULL REFERENCES posting (id),
    -- payable: the company owes the party. receivable: the party owes the company.
    kind text NOT NULL CHECK (kind IN ('payable', 'receivable')),
    -- Who: an entity name such as 'vendor' or 'customer', and its record.
    party_type text NOT NULL CHECK (party_type <> ''),
    party_id uuid NOT NULL,
    -- The general ledger account that carries this debt.
    account_id uuid NOT NULL REFERENCES account (id),
    -- In the company's accounting currency.
    amount numeric(20, 4) NOT NULL CHECK (amount > 0),
    -- How much of it has been settled. Kept by the settlement trigger.
    settled numeric(20, 4) NOT NULL DEFAULT 0,
    doc_date date NOT NULL,
    due_date date,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT settled_within_amount CHECK (settled >= 0 AND settled <= amount)
);
CREATE INDEX open_item_party ON open_item (company_id, kind, party_type, party_id);
CREATE INDEX open_item_posting ON open_item (posting_id);

CREATE FUNCTION open_item_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
        IF pst.company_id <> NEW.company_id THEN
            RAISE EXCEPTION 'an open item must carry the company of its posting'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NOT EXISTS (SELECT 1 FROM account WHERE id = NEW.account_id AND company_id = NEW.company_id) THEN
            RAISE EXCEPTION 'the account of an open item must belong to its company'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.settled <> 0 THEN
            RAISE EXCEPTION 'an open item starts unsettled' USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    -- Only the settlement trigger may change an item, and only how much
    -- of it is settled.
    IF TG_OP = 'UPDATE' AND pg_trigger_depth() >= 2
        AND (to_jsonb(NEW) - 'settled') = (to_jsonb(OLD) - 'settled') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'open items change only through settlements'
        USING ERRCODE = 'check_violation';
END $$;
CREATE TRIGGER open_item_guard BEFORE INSERT OR UPDATE OR DELETE ON open_item
    FOR EACH ROW EXECUTE FUNCTION open_item_guard();
CREATE TRIGGER open_item_no_truncate BEFORE TRUNCATE ON open_item
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();

-- One posting settling part or all of one open item.
CREATE TABLE settlement (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- The posting that settles: a payment, or the reversal of the
    -- posting that created the item.
    posting_id uuid NOT NULL REFERENCES posting (id),
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    -- Positive: settles. Negative: takes a settlement back.
    amount numeric(20, 4) NOT NULL CHECK (amount <> 0),
    -- The item was called off because its document was reversed. Nothing
    -- was paid.
    cancels boolean NOT NULL DEFAULT false,
    -- UNIQUE: a settlement can be reversed at most once.
    reverses_id uuid UNIQUE REFERENCES settlement (id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX settlement_item ON settlement (open_item_id);
CREATE INDEX settlement_posting ON settlement (posting_id);

CREATE FUNCTION settlement_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    item open_item%ROWTYPE;
    orig settlement%ROWTYPE;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    -- The row lock makes settlements of one item happen one after the other.
    SELECT * INTO item FROM open_item WHERE id = NEW.open_item_id FOR UPDATE;
    IF pst.company_id <> NEW.company_id OR item.company_id <> NEW.company_id THEN
        RAISE EXCEPTION 'a settlement, its posting and its open item must belong to one company'
            USING ERRCODE = 'check_violation';
    END IF;

    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM settlement WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        IF orig.open_item_id <> NEW.open_item_id THEN
            RAISE EXCEPTION 'a reversal must be for the open item of the settlement it reverses'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.amount := -orig.amount;
        NEW.cancels := orig.cancels;
    ELSIF NEW.amount <= 0 THEN
        RAISE EXCEPTION 'a settlement must be for more than zero' USING ERRCODE = 'check_violation';
    ELSIF NEW.cancels THEN
        IF item.settled <> 0 THEN
            RAISE EXCEPTION 'this has been paid in part or in full; reverse the payment first'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.amount := item.amount;
    ELSIF item.settled + NEW.amount > item.amount THEN
        RAISE EXCEPTION 'only % is still open, not %',
            trim_scale(item.amount - item.settled), trim_scale(NEW.amount)
            USING ERRCODE = 'check_violation';
    END IF;

    UPDATE open_item SET settled = settled + NEW.amount WHERE id = NEW.open_item_id;
    RETURN NEW;
END $$;
CREATE TRIGGER settlement_before_insert BEFORE INSERT ON settlement
    FOR EACH ROW EXECUTE FUNCTION settlement_before_insert();
CREATE TRIGGER settlement_immutable BEFORE UPDATE OR DELETE ON settlement
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER settlement_no_truncate BEFORE TRUNCATE ON settlement
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
