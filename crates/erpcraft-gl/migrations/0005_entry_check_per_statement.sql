-- The rules of a ledger entry, checked once per statement instead of once
-- per row.
--
-- gl_entry_before_insert looked up the posting, the account, the fiscal
-- period, the company and two currencies again for every row, though a
-- posting's entries arrive in one statement and most of those answers are
-- the same for all of them. This trigger sees the statement's rows as a
-- table and checks them with one query, and takes the lock on the fiscal
-- period once per company and date.
--
-- The rules are the same, for every writer, raw SQL included:
--   - an entry carries the company and date of its posting;
--   - its account belongs to the company, is not a group account, and is
--     active;
--   - its date falls in an open fiscal period;
--   - its amounts fit the decimals of their currency, an entry in the
--     accounting currency has rate 1, and the accounting amount is the
--     transaction amount times the rate, rounded.
--
-- What differs from a row trigger: the rows are already in the table when
-- a rule is found broken. The statement fails all the same and takes its
-- rows with it. One statement may hold entries of several postings,
-- companies and dates; nothing here assumes otherwise. Where several rows
-- break a rule, the one reported is the first by posting and line.
CREATE FUNCTION gl_entry_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    bad record;
    day record;
BEGIN
    -- The first entry that breaks a rule, and which rule, in the order the
    -- rules are listed above. Foreign keys guarantee the joins find their
    -- rows; a row that points nowhere is refused by its foreign key.
    SELECT * INTO bad FROM (
        SELECT e.posting_id, e.line_no, e.currency, a.code AS account,
               b.decimals AS base_decimals,
               CASE
                   WHEN p.company_id <> e.company_id OR p.posting_date <> e.posting_date
                       THEN 'posting'
                   WHEN a.company_id <> e.company_id THEN 'foreign account'
                   WHEN a.is_group THEN 'group account'
                   WHEN NOT a.active THEN 'inactive account'
                   WHEN e.txn_debit <> round(e.txn_debit, t.decimals)
                       OR e.txn_credit <> round(e.txn_credit, t.decimals) THEN 'decimals'
                   WHEN e.currency = c.base_currency AND e.fx_rate <> 1 THEN 'rate'
                   WHEN e.debit <> round(e.txn_debit * e.fx_rate, b.decimals)
                       OR e.credit <> round(e.txn_credit * e.fx_rate, b.decimals) THEN 'amount'
               END AS problem
        FROM inserted e
        JOIN posting p ON p.id = e.posting_id
        JOIN account a ON a.id = e.account_id
        JOIN company c ON c.id = e.company_id
        JOIN currency b ON b.code = c.base_currency
        JOIN currency t ON t.code = e.currency
    ) checked
    WHERE problem IS NOT NULL
    ORDER BY posting_id, line_no
    LIMIT 1;

    IF bad.problem = 'posting' THEN
        RAISE EXCEPTION 'a ledger entry must carry the company and date of its posting (line % of posting %)',
            bad.line_no, bad.posting_id USING ERRCODE = 'check_violation';
    ELSIF bad.problem = 'foreign account' THEN
        RAISE EXCEPTION 'account % belongs to another company', bad.account
            USING ERRCODE = 'check_violation';
    ELSIF bad.problem = 'group account' THEN
        RAISE EXCEPTION 'account % is a group account and cannot be posted to', bad.account
            USING ERRCODE = 'check_violation';
    ELSIF bad.problem = 'inactive account' THEN
        RAISE EXCEPTION 'account % is inactive', bad.account USING ERRCODE = 'check_violation';
    END IF;

    -- FOR SHARE makes posting and closing a period wait for each other: a
    -- close that commits first is seen here, and a close that starts later
    -- waits until this transaction ends. It is the statement
    -- gl_entry_before_insert ran, once per company and date instead of
    -- once per row. A close that slips in between the insert of the rows
    -- and this lock is waited for and then seen, so the statement fails.
    -- (gl_posting_period_check takes the same lock when the posting is
    -- written; this one also covers entries added to an older posting.)
    FOR day IN SELECT DISTINCT company_id, posting_date FROM inserted ORDER BY 1, 2 LOOP
        PERFORM 1 FROM fiscal_period
            WHERE company_id = day.company_id AND status = 'open'
              AND day.posting_date BETWEEN start_date AND end_date
            FOR SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION 'no open fiscal period contains %', day.posting_date
                USING ERRCODE = 'check_violation';
        END IF;
    END LOOP;

    IF bad.problem = 'decimals' THEN
        RAISE EXCEPTION 'an amount has more decimal places than % allows (line % of posting %)',
            bad.currency, bad.line_no, bad.posting_id USING ERRCODE = 'check_violation';
    ELSIF bad.problem = 'rate' THEN
        RAISE EXCEPTION 'an entry in the accounting currency must have an exchange rate of 1 (line % of posting %)',
            bad.line_no, bad.posting_id USING ERRCODE = 'check_violation';
    ELSIF bad.problem = 'amount' THEN
        RAISE EXCEPTION 'the accounting amount must equal the transaction amount times the exchange rate, rounded to % decimals (line % of posting %)',
            bad.base_decimals, bad.line_no, bad.posting_id USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END $$;

DROP TRIGGER gl_entry_before_insert ON gl_entry;
CREATE TRIGGER gl_entry_check AFTER INSERT ON gl_entry
    REFERENCING NEW TABLE AS inserted
    FOR EACH STATEMENT EXECUTE FUNCTION gl_entry_check();

-- The old function stays, unattached, as the record of what was enforced
-- before.
COMMENT ON FUNCTION gl_entry_before_insert() IS
    'No longer attached to gl_entry: replaced by gl_entry_check, which enforces the same rules once per statement.';
