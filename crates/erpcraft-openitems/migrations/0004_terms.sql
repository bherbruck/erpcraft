-- Payment terms: when an invoice is due, and the discount for paying it
-- early. Shared by all companies, like tax codes. A vendor or customer
-- names its usual terms; an order takes them from its party and an
-- invoice from its order, each able to name others.
--
-- Due: the invoice's date plus net_days, or with end_of_month the last
-- day of the invoice's month plus net_days. A discount, when the terms
-- give one, is discount_percent of what is owed, for paying within
-- discount_days counted the same way.
CREATE TABLE payment_terms (
    id uuid PRIMARY KEY,
    code text NOT NULL UNIQUE CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    net_days integer NOT NULL DEFAULT 0 CHECK (net_days >= 0 AND net_days <= 3660),
    end_of_month boolean NOT NULL DEFAULT false,
    discount_percent numeric(9, 6) CHECK (discount_percent > 0 AND discount_percent < 100),
    discount_days integer CHECK (discount_days >= 0),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT payment_terms_discount_whole
        CHECK ((discount_percent IS NULL) = (discount_days IS NULL)),
    CONSTRAINT payment_terms_discount_before_due
        CHECK (discount_days IS NULL OR discount_days <= net_days)
);

-- The early-payment discount an item offers: until discount_date, the
-- party may pay it less discount_amount (in the item's currency) and have
-- it settled in full. Only a debt offers one.
ALTER TABLE open_item
    ADD COLUMN discount_date date,
    ADD COLUMN discount_amount numeric(20, 4),
    ADD CONSTRAINT open_item_discount_whole
        CHECK ((discount_date IS NULL) = (discount_amount IS NULL)),
    ADD CONSTRAINT open_item_discount_within
        CHECK (discount_amount IS NULL OR (discount_amount > 0 AND discount_amount < txn_amount));

-- Aging as of a date reads settlements by the date of their posting.
CREATE INDEX open_item_company_kind ON open_item (company_id, kind, doc_date);
