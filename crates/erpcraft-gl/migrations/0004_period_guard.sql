-- Two holes around closed periods.
--
-- 1. The dates of a period could be changed after the fact. Shorten a
--    closed year, create an open period over the freed dates, and entries
--    go into that year without anyone reopening it; what a year-end close
--    covered is no longer what it closed.
--
-- 2. Only general ledger entries were held to open periods. A posting
--    that writes none, such as a stock transfer, could be dated in a
--    closed year.

-- The dates of a period are fixed while it is closed, and from the moment
-- anything is posted inside them. A period in either state cannot be
-- deleted. An empty open period can still be corrected or removed.
--
-- "Anything posted" is any posting of the company dated in the range,
-- not only one with general ledger entries: every ledger entry carries
-- the date of its posting, so this covers the entries of every ledger.
CREATE FUNCTION gl_fiscal_period_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    change text;
BEGIN
    IF TG_OP = 'DELETE' THEN
        change := 'it cannot be deleted';
    ELSIF NEW.start_date <> OLD.start_date OR NEW.end_date <> OLD.end_date
        OR NEW.company_id <> OLD.company_id THEN
        change := 'its dates cannot change';
    ELSE
        RETURN NEW;
    END IF;
    IF OLD.status = 'closed' THEN
        RAISE EXCEPTION 'fiscal period % is closed: %', OLD.name, change
            USING ERRCODE = 'check_violation';
    END IF;
    -- The row is locked by now, and a posting takes a share lock on its
    -- period before it is written (below), so none can appear meanwhile.
    IF EXISTS (SELECT 1 FROM posting
               WHERE company_id = OLD.company_id
                 AND posting_date BETWEEN OLD.start_date AND OLD.end_date) THEN
        RAISE EXCEPTION 'fiscal period % already has postings: %', OLD.name, change
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER fiscal_period_guard BEFORE UPDATE OR DELETE ON fiscal_period
    FOR EACH ROW EXECUTE FUNCTION gl_fiscal_period_guard();
CREATE INDEX posting_company_date ON posting (company_id, posting_date);

-- A posting cannot be dated in a closed period, whichever ledgers it then
-- writes to. The posting table is the kernel's; the general ledger guards
-- it because fiscal periods are the general ledger's, and a period that is
-- closed is closed for every ledger.
--
-- A date in no period at all is let through here: such a posting cannot
-- have general ledger entries (they need an open period), and a company
-- that keeps stock without keeping books has no periods.
--
-- FOR SHARE is the same lock gl_entry_before_insert takes: a close that
-- commits first is seen here, and a close that starts later waits until
-- this transaction ends.
CREATE FUNCTION gl_posting_period_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    period fiscal_period%ROWTYPE;
BEGIN
    SELECT * INTO period FROM fiscal_period
        WHERE company_id = NEW.company_id
          AND NEW.posting_date BETWEEN start_date AND end_date
        FOR SHARE;
    IF FOUND AND period.status <> 'open' THEN
        RAISE EXCEPTION 'no open fiscal period contains %', NEW.posting_date
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER posting_period_check BEFORE INSERT ON posting
    FOR EACH ROW EXECUTE FUNCTION gl_posting_period_check();
