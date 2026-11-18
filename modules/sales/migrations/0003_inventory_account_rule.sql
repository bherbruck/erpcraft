-- The inventory account is no longer a sales setting: every module that
-- moves stock asks the general ledger's account rules for it, so they
-- cannot disagree. What was set here becomes the company's default rule.
-- Where the company already has one, from purchasing or made by hand, it
-- must name the same account.

-- This migration has not been committed or released, and the only
-- database it was applied to (the development one) had the same account
-- everywhere, so it was corrected in place rather than followed by another.
--
-- A company whose sales inventory account differs from the one already
-- chosen for it must not be switched silently: its stock would go on
-- posting, to another account, and the old balance would stay behind. The
-- upgrade stops and says what to do. Nothing of this migration is applied
-- then, and the upgrade can be run again.
DO $$
DECLARE
    clash record;
BEGIN
    SELECT c.code AS company, mine.code AS mine, theirs.code AS theirs INTO clash
    FROM sales_settings s
    JOIN account_rule r ON r.company_id = s.company_id AND r.purpose = 'inventory'
                       AND r.scope_type IS NULL
    JOIN company c ON c.id = s.company_id
    JOIN account mine ON mine.id = s.inventory_account_id
    JOIN account theirs ON theirs.id = r.account_id
    WHERE r.account_id <> s.inventory_account_id
    ORDER BY c.code
    LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'company %: sales posts stock to inventory account %, but the inventory account of the company is %. Upgrading would silently switch it to the other account and leave the balance behind. Before upgrading, make the two agree and move the balance with a journal entry; then upgrade again.',
            clash.company, clash.mine, clash.theirs;
    END IF;
END $$;

-- Only a rule for the same account can be in the way by now.
INSERT INTO account_rule (id, company_id, purpose, account_id)
SELECT gen_random_uuid(), company_id, 'inventory', inventory_account_id FROM sales_settings
ON CONFLICT DO NOTHING;

CREATE OR REPLACE FUNCTION sales_settings_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM account
        WHERE company_id = NEW.company_id
          AND id IN (NEW.receivable_account_id, NEW.revenue_account_id, NEW.cogs_account_id))
       <> (SELECT count(DISTINCT a) FROM unnest(ARRAY[NEW.receivable_account_id,
                     NEW.revenue_account_id, NEW.cogs_account_id]) AS a) THEN
        RAISE EXCEPTION 'the sales accounts must belong to the company'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

ALTER TABLE sales_settings DROP COLUMN inventory_account_id;
