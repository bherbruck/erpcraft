-- Which flow of the cash flow statement an account's changes belong to.
-- Null: the statement decides by the account's type (see "Cash flow
-- statement" in ARCHITECTURE.md). Income and expense accounts have none:
-- they make up the net income.
ALTER TABLE account ADD COLUMN cash_flow text
    CHECK (cash_flow IN ('cash', 'operating', 'investing', 'financing'));
ALTER TABLE account ADD CONSTRAINT account_cash_flow_balance_sheet
    CHECK (cash_flow IS NULL OR account_type IN ('asset', 'liability', 'equity'));
