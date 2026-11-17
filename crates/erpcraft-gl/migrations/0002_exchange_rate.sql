-- Exchange rates on record. A posting in a foreign currency that states no
-- rate uses the latest one dated on or before its posting date. The rate
-- is copied onto each ledger entry, so changing a rate here never changes
-- what was already posted.

CREATE TABLE exchange_rate (
    id uuid PRIMARY KEY,
    from_currency text NOT NULL REFERENCES currency (code),
    to_currency text NOT NULL REFERENCES currency (code),
    -- The first day the rate applies.
    rate_date date NOT NULL,
    -- Units of to_currency per unit of from_currency.
    rate numeric(20, 10) NOT NULL CHECK (rate > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (from_currency, to_currency, rate_date),
    CHECK (from_currency <> to_currency)
);
