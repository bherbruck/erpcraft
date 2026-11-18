-- A fiscal year kept as several periods (twelve months, thirteen
-- four-week periods): a year-end close names the year's first period as
-- well as its last, which it is dated in, and closes the income and
-- expenses of all of them together. Without a first period the year is
-- the one period, as before.
ALTER TABLE year_end_close ADD COLUMN first_period_id uuid REFERENCES fiscal_period (id);
