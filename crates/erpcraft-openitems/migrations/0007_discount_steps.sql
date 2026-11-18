-- Payment terms with more than one early-payment discount: 2 % within 10
-- days, 1 % within 20, net 30. The terms' own discount is the first step;
-- further steps are rows of their own.
CREATE TABLE payment_terms_step (
    id uuid PRIMARY KEY,
    terms_id uuid NOT NULL REFERENCES payment_terms (id) ON DELETE CASCADE,
    discount_percent numeric(9, 6) NOT NULL
        CHECK (discount_percent > 0 AND discount_percent < 100),
    discount_days integer NOT NULL CHECK (discount_days >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (terms_id, discount_days)
);

-- Every step an item offers: until which day, how much off, in the item's
-- currency. The item's discount_date and discount_amount are the first.
CREATE TABLE open_item_discount (
    open_item_id uuid NOT NULL REFERENCES open_item (id),
    until date NOT NULL,
    amount numeric(20, 4) NOT NULL CHECK (amount > 0),
    PRIMARY KEY (open_item_id, until)
);
CREATE TRIGGER open_item_discount_immutable BEFORE UPDATE OR DELETE ON open_item_discount
    FOR EACH ROW EXECUTE FUNCTION erp_immutable();
CREATE TRIGGER open_item_discount_no_truncate BEFORE TRUNCATE ON open_item_discount
    FOR EACH STATEMENT EXECUTE FUNCTION erp_immutable();
