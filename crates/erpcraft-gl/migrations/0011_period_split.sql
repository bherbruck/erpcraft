-- A year divided into months (fiscal_period.split_months).
--
-- The year's row is deleted and its months inserted in one transaction.
-- The guard of 0004 refuses deleting a period with postings in its dates,
-- and the months cannot be inserted first (fiscal_period_no_overlap). The
-- division changes nothing a posting depends on: the months cover the
-- year's dates exactly, open as the year was, and postings are dated, not
-- filed by period. So the guard lets an open period with postings be
-- deleted when the transaction says it is dividing that very period.
--
-- The command sets erpcraft.period_split, transaction-local, to the id of
-- the period it divides. A closed period, and a change of dates, are
-- refused as before; a year-end close that refers to the period still
-- stops the delete by its foreign key.
CREATE OR REPLACE FUNCTION gl_fiscal_period_guard() RETURNS trigger LANGUAGE plpgsql AS $$
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
    IF TG_OP = 'DELETE'
       AND current_setting('erpcraft.period_split', true) = OLD.id::text THEN
        RETURN OLD;
    END IF;
    -- The row is locked by now, and a posting takes a share lock on its
    -- period before it is written, so none can appear meanwhile.
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

-- The setting alone would let anyone who can run SQL delete a period with
-- postings. So a period deleted under it must be divided, not lost: when
-- the transaction commits, its dates must be covered, with no gap, by open
-- periods of the same company that lie within them. A mistaken or partial
-- division cannot commit.
CREATE FUNCTION gl_fiscal_period_split_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    covered integer;
    outside integer;
BEGIN
    IF current_setting('erpcraft.period_split', true) IS DISTINCT FROM OLD.id::text THEN
        RETURN NULL;
    END IF;
    SELECT COALESCE(sum(LEAST(end_date, OLD.end_date) - GREATEST(start_date, OLD.start_date) + 1), 0),
           count(*) FILTER (WHERE start_date < OLD.start_date OR end_date > OLD.end_date
                               OR status <> 'open')
        INTO covered, outside
        FROM fiscal_period
        WHERE company_id = OLD.company_id
          AND start_date <= OLD.end_date AND end_date >= OLD.start_date;
    IF outside > 0 OR covered <> OLD.end_date - OLD.start_date + 1 THEN
        RAISE EXCEPTION 'fiscal period % was deleted to be divided, but open periods within its dates do not cover them', OLD.name
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER fiscal_period_split_check AFTER DELETE ON fiscal_period
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION gl_fiscal_period_split_check();
