-- Budgets: what a company plans, by account, month and optionally a
-- dimension value. See "Budgets" in ARCHITECTURE.md.

CREATE TABLE budget (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    starts_on date NOT NULL CHECK (extract(day FROM starts_on) = 1),
    ends_on date NOT NULL CHECK (ends_on = (date_trunc('month', ends_on) + interval '1 month - 1 day')::date),
    version text NOT NULL DEFAULT 'original' CHECK (version ~ '^[a-z][a-z0-9_]*$'),
    -- The dimension the lines are kept by, if any.
    dimension_id uuid REFERENCES dimension (id),
    locked boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    UNIQUE (company_id, starts_on, version),
    CONSTRAINT budget_span CHECK (ends_on > starts_on AND ends_on < starts_on + interval '24 months')
);

CREATE TABLE budget_line (
    id uuid PRIMARY KEY,
    budget_id uuid NOT NULL REFERENCES budget (id) ON DELETE CASCADE,
    account_id uuid NOT NULL REFERENCES account (id),
    -- The first day of the month.
    month date NOT NULL CHECK (extract(day FROM month) = 1),
    dimension_value_id uuid REFERENCES dimension_value (id),
    -- In the account's natural direction: a cost as a positive expense, a
    -- sale as positive income. Accounting currency.
    amount numeric(20, 4) NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT budget_line_once UNIQUE NULLS NOT DISTINCT
        (budget_id, account_id, month, dimension_value_id)
);
CREATE INDEX budget_line_account ON budget_line (account_id);

-- A line is of the budget's company, months and dimension, and a locked
-- budget's lines do not change.
CREATE FUNCTION budget_line_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    b budget%ROWTYPE;
BEGIN
    SELECT * INTO b FROM budget WHERE id = COALESCE(NEW.budget_id, OLD.budget_id);
    IF NOT FOUND THEN
        RETURN COALESCE(NEW, OLD);  -- the budget is being deleted
    END IF;
    IF b.locked THEN
        RAISE EXCEPTION 'budget % is locked', b.code USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM account WHERE id = NEW.account_id AND company_id = b.company_id
                   AND NOT is_group) THEN
        RAISE EXCEPTION 'a budget line is for an account of its company that can be posted to'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.month < b.starts_on OR NEW.month > b.ends_on THEN
        RAISE EXCEPTION 'month % is outside budget %', to_char(NEW.month, 'YYYY-MM'), b.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.dimension_value_id IS NOT NULL AND (b.dimension_id IS NULL OR NOT EXISTS (
        SELECT 1 FROM dimension_value WHERE id = NEW.dimension_value_id
          AND dimension_id = b.dimension_id)) THEN
        RAISE EXCEPTION 'a budget line''s dimension value is of the budget''s dimension'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER budget_line_check BEFORE INSERT OR UPDATE OR DELETE ON budget_line
    FOR EACH ROW EXECUTE FUNCTION budget_line_check();

-- A locked budget changes only by being unlocked; its dimension and dates
-- do not change once it has lines.
CREATE FUNCTION budget_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.locked THEN
            RAISE EXCEPTION 'budget % is locked', OLD.code USING ERRCODE = 'check_violation';
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.locked AND NEW.locked THEN
        RAISE EXCEPTION 'budget % is locked', OLD.code USING ERRCODE = 'check_violation';
    END IF;
    IF (NEW.dimension_id IS DISTINCT FROM OLD.dimension_id OR NEW.starts_on <> OLD.starts_on
        OR NEW.ends_on <> OLD.ends_on OR NEW.company_id <> OLD.company_id)
       AND EXISTS (SELECT 1 FROM budget_line WHERE budget_id = OLD.id) THEN
        RAISE EXCEPTION 'budget % has lines: its company, dates and dimension stay', OLD.code
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.dimension_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM dimension WHERE id = NEW.dimension_id AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'a budget is kept by a dimension of its company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER budget_guard BEFORE UPDATE OR DELETE ON budget
    FOR EACH ROW EXECUTE FUNCTION budget_guard();
CREATE FUNCTION budget_insert_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.dimension_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM dimension WHERE id = NEW.dimension_id AND company_id = NEW.company_id) THEN
        RAISE EXCEPTION 'a budget is kept by a dimension of its company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER budget_insert_check BEFORE INSERT ON budget
    FOR EACH ROW EXECUTE FUNCTION budget_insert_check();
