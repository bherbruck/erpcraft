-- Who sells a quote; the order a won quote makes takes it.
ALTER TABLE sales_quote ADD COLUMN salesperson_id uuid REFERENCES sales_rep (id);
