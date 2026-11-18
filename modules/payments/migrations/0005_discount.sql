-- Early-payment discount. An allocation that pays the rest of an item less
-- the discount its terms offer settles the item in full: amount is the
-- money paid for it, discount what the party keeps back (both in the
-- item's currency). The discount is posted to the discount account and,
-- where the company corrects tax for it, takes the tax on it back.
ALTER TABLE payment_allocation
    ADD COLUMN discount numeric(20, 4) NOT NULL DEFAULT 0 CHECK (discount >= 0);
