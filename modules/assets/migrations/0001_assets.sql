-- Fixed assets: what a company owns and uses for longer than a year, what
-- it cost, and how it is depreciated month by month until it is sold or
-- scrapped. The general ledger holds the money; these tables hold the
-- register that explains it.

-- How a company depreciates, one row per company. Without a row: a full
-- month in the month an asset goes into service, and no minimum.
CREATE TABLE asset_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    -- full_month: the month an asset goes into service is depreciated in
    -- full. next_month: depreciation starts the month after.
    convention text NOT NULL DEFAULT 'full_month'
        CHECK (convention IN ('full_month', 'next_month')),
    -- Below this cost an asset is not capitalised, unless the company's
    -- policy (asset.below_minimum) allows it. 0: no minimum.
    minimum_cost numeric(20, 4) NOT NULL DEFAULT 0 CHECK (minimum_cost >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The convention decides which months are depreciated: once a company has
-- depreciated anything, changing it would change what was posted.
CREATE FUNCTION asset_settings_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    -- Without settings the convention was full_month.
    IF TG_OP = 'UPDATE' AND NEW.company_id <> OLD.company_id THEN
        RAISE EXCEPTION 'settings stay with their company' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.convention <> (CASE WHEN TG_OP = 'UPDATE' THEN OLD.convention ELSE 'full_month' END)
       AND EXISTS (SELECT 1 FROM asset_depreciation WHERE company_id = NEW.company_id
                   AND NOT reversed) THEN
        RAISE EXCEPTION 'the company has depreciated assets under its convention, which no longer changes'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER asset_settings_check BEFORE INSERT OR UPDATE ON asset_settings
    FOR EACH ROW EXECUTE FUNCTION asset_settings_check();

-- A kind of asset of a company, with how it is depreciated unless an asset
-- says otherwise. Its accounts are account rules with scope asset_class.
CREATE TABLE asset_class (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    method text NOT NULL DEFAULT 'straight_line'
        CHECK (method IN ('straight_line', 'declining_balance', 'none')),
    -- For declining_balance: percent of the book value per year.
    declining_rate numeric(7, 4) CHECK (declining_rate > 0 AND declining_rate <= 100),
    useful_life_months integer CHECK (useful_life_months BETWEEN 1 AND 1200),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CHECK (method <> 'declining_balance' OR declining_rate IS NOT NULL),
    CHECK (method = 'none' OR useful_life_months IS NOT NULL)
);

CREATE TABLE asset (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    class_id uuid NOT NULL REFERENCES asset_class (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    -- planned: not capitalised or not in service yet. Changed only by the
    -- module's commands.
    status text NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'in_service', 'fully_depreciated', 'disposed')),
    -- From the class unless given.
    method text NOT NULL CHECK (method IN ('straight_line', 'declining_balance', 'none')),
    declining_rate numeric(7, 4) CHECK (declining_rate > 0 AND declining_rate <= 100),
    useful_life_months integer CHECK (useful_life_months BETWEEN 1 AND 1200),
    salvage_value numeric(20, 4) NOT NULL DEFAULT 0 CHECK (salvage_value >= 0),
    -- In the accounting currency, set once, by asset.capitalise or
    -- asset.take_over.
    cost numeric(20, 4) CHECK (cost > 0),
    acquisition_date date,
    -- The day from which the books hold the asset: the date of the posting
    -- it was capitalised from, or the opening date of an asset taken over.
    recorded_on date,
    in_service_date date,
    -- The ledger entry the asset was capitalised from: an entry is
    -- capitalised once.
    source_entry_id uuid UNIQUE REFERENCES gl_entry (id),
    -- For an asset taken over from another system: its depreciation up to
    -- the end of the month of opening_through.
    opening_accumulated numeric(20, 4) NOT NULL DEFAULT 0 CHECK (opening_accumulated >= 0),
    opening_through date,
    disposed_on date,
    disposal_id uuid REFERENCES document (id),
    location text,
    cost_centre text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    CHECK (method <> 'declining_balance' OR declining_rate IS NOT NULL),
    CHECK (method = 'none' OR useful_life_months IS NOT NULL),
    CHECK (cost IS NULL OR salvage_value + opening_accumulated <= cost),
    CHECK ((cost IS NULL) = (recorded_on IS NULL)),
    CHECK (status = 'planned' OR (cost IS NOT NULL AND in_service_date IS NOT NULL)
           OR (status = 'disposed' AND cost IS NOT NULL)),
    CHECK ((status = 'disposed') = (disposed_on IS NOT NULL)),
    CHECK (in_service_date IS NULL OR acquisition_date IS NULL
           OR in_service_date >= acquisition_date)
);
CREATE INDEX asset_class_of ON asset (class_id);

-- An asset takes its class's depreciation unless it says otherwise; the
-- class is one of its own company. Once the asset has been depreciated,
-- what its depreciation was worked out from no longer changes, and a cost
-- is set only once.
CREATE FUNCTION asset_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    cls asset_class%ROWTYPE;
BEGIN
    SELECT * INTO cls FROM asset_class WHERE id = NEW.class_id;
    IF cls.company_id IS DISTINCT FROM NEW.company_id THEN
        RAISE EXCEPTION 'the asset class must be one of the asset''s company'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.method IS NULL THEN
            NEW.method := cls.method;
            NEW.declining_rate := COALESCE(NEW.declining_rate, cls.declining_rate);
        END IF;
        NEW.useful_life_months := COALESCE(NEW.useful_life_months, cls.useful_life_months);
        RETURN NEW;
    END IF;
    IF OLD.cost IS NOT NULL AND NEW.cost IS DISTINCT FROM OLD.cost THEN
        RAISE EXCEPTION 'the asset''s cost was capitalised and does not change'
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.cost IS NOT NULL AND NEW.class_id <> OLD.class_id THEN
        RAISE EXCEPTION 'a capitalised asset stays in its class: its class says which accounts hold it'
            USING ERRCODE = 'check_violation';
    END IF;
    IF (NEW.method, NEW.declining_rate, NEW.useful_life_months, NEW.salvage_value,
        NEW.in_service_date, NEW.opening_accumulated, NEW.opening_through)
       IS DISTINCT FROM
       (OLD.method, OLD.declining_rate, OLD.useful_life_months, OLD.salvage_value,
        OLD.in_service_date, OLD.opening_accumulated, OLD.opening_through)
       AND EXISTS (SELECT 1 FROM asset_depreciation WHERE asset_id = NEW.id AND NOT reversed) THEN
        RAISE EXCEPTION 'the asset has been depreciated: its method, life, rate, salvage value and dates no longer change'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

-- A capitalised asset is in the books: it is disposed of, not deleted.
CREATE FUNCTION asset_delete_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.cost IS NOT NULL THEN
        RAISE EXCEPTION 'a capitalised asset cannot be deleted; dispose of it instead'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN OLD;
END $$;
CREATE TRIGGER asset_delete_check BEFORE DELETE ON asset
    FOR EACH ROW EXECUTE FUNCTION asset_delete_check();

-- One row per asset and month depreciated, by a depreciation run or by a
-- disposal catching up to its date. A month is depreciated once: a second
-- run for the same month finds it taken. A reversal marks the rows of what
-- it reversed; nothing else changes them.
CREATE TABLE asset_depreciation (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    asset_id uuid NOT NULL REFERENCES asset (id),
    document_id uuid NOT NULL REFERENCES document (id),
    month date NOT NULL CHECK (extract(day FROM month) = 1),
    posting_date date NOT NULL,
    amount numeric(20, 4) NOT NULL CHECK (amount > 0),
    reversed boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX asset_depreciation_once ON asset_depreciation (asset_id, month)
    WHERE NOT reversed;
CREATE INDEX asset_depreciation_document ON asset_depreciation (document_id);

CREATE TRIGGER asset_check BEFORE INSERT OR UPDATE ON asset
    FOR EACH ROW EXECUTE FUNCTION asset_check();

CREATE FUNCTION asset_depreciation_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'depreciation is reversed, not deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.reversed OR NOT NEW.reversed
       OR (NEW.id, NEW.company_id, NEW.asset_id, NEW.document_id, NEW.month, NEW.posting_date, NEW.amount)
          IS DISTINCT FROM
          (OLD.id, OLD.company_id, OLD.asset_id, OLD.document_id, OLD.month, OLD.posting_date, OLD.amount) THEN
        RAISE EXCEPTION 'depreciation is only ever marked reversed' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER asset_depreciation_guard BEFORE UPDATE OR DELETE ON asset_depreciation
    FOR EACH ROW EXECUTE FUNCTION asset_depreciation_guard();

-- The periods a company has run depreciation for, one row per posted run
-- that is not reversed. Runs go forward: none for a period already run, and
-- none for a period before one that was.
CREATE TABLE depreciation_period (
    company_id uuid NOT NULL REFERENCES company (id),
    period_end date NOT NULL CHECK (period_end + 1 = date_trunc('month', period_end + 1)::date),
    document_id uuid NOT NULL UNIQUE REFERENCES document (id),
    PRIMARY KEY (company_id, period_end)
);

CREATE FUNCTION depreciation_period_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    later date;
BEGIN
    -- One run at a time per company.
    PERFORM pg_advisory_xact_lock(hashtext('depreciation_period'), hashtext(NEW.company_id::text));
    SELECT max(period_end) INTO later FROM depreciation_period
     WHERE company_id = NEW.company_id AND period_end >= NEW.period_end;
    IF later IS NOT NULL THEN
        RAISE EXCEPTION 'depreciation has been run for % already; a run goes after the last one', later
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER depreciation_period_check BEFORE INSERT ON depreciation_period
    FOR EACH ROW EXECUTE FUNCTION depreciation_period_check();

-- What a depreciation run posted, one line per asset.
CREATE TABLE depreciation_run_line (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES document (id) ON DELETE CASCADE,
    line_no integer NOT NULL,
    asset_id uuid NOT NULL REFERENCES asset (id),
    first_month date NOT NULL,
    last_month date NOT NULL,
    months integer NOT NULL CHECK (months > 0),
    amount numeric(20, 4) NOT NULL CHECK (amount > 0),
    expense_account_id uuid NOT NULL REFERENCES account (id),
    accumulated_account_id uuid NOT NULL REFERENCES account (id),
    UNIQUE (document_id, line_no),
    UNIQUE (document_id, asset_id)
);
CREATE TRIGGER depreciation_run_line_guard BEFORE INSERT OR UPDATE OR DELETE ON depreciation_run_line
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();

-- Selling or scrapping an asset, and what it came to once posted.
CREATE TABLE asset_disposal (
    document_id uuid PRIMARY KEY REFERENCES document (id) ON DELETE CASCADE,
    asset_id uuid NOT NULL REFERENCES asset (id),
    -- What the asset was sold for, in the accounting currency; 0 when
    -- scrapped.
    proceeds numeric(20, 4) NOT NULL DEFAULT 0 CHECK (proceeds >= 0),
    -- Where the proceeds are posted: a bank, or a clearing account a sales
    -- invoice to the buyer settles.
    proceeds_account_id uuid REFERENCES account (id),
    -- Set when posted.
    depreciation numeric(20, 4),
    cost numeric(20, 4),
    accumulated numeric(20, 4),
    book_value numeric(20, 4),
    gain numeric(20, 4),
    CHECK (proceeds = 0 OR proceeds_account_id IS NOT NULL)
);
CREATE INDEX asset_disposal_asset ON asset_disposal (asset_id);
CREATE TRIGGER asset_disposal_guard BEFORE INSERT OR UPDATE OR DELETE ON asset_disposal
    FOR EACH ROW EXECUTE FUNCTION erp_document_child_guard();
