-- What the customer calls its order (its purchase order number) and the
-- day it asks for the goods, both optional: an order taken by phone may
-- have neither. EDI fills them from the 850's BEG03 and its requested
-- delivery date, which until now went into the memo.
ALTER TABLE sales_order
    ADD COLUMN customer_reference text CHECK (customer_reference <> '' AND length(customer_reference) <= 100),
    ADD COLUMN requested_date date;
