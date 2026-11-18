-- Manufacturing no longer has accounts of its own: the general ledger's
-- account rules say which inventory account each item posts to. What was
-- set here is carried over: the materials account becomes the company's
-- default, and where finished goods went to another account than that
-- default, every product of a bill of materials gets a rule of its own.
--
-- Only bills that exist now get such a rule. A bill created later posts
-- its product to the company's default inventory account, unless someone
-- adds an account rule for that item: nothing remembers that this company
-- once kept finished goods apart.

-- This migration has not been committed or released, and the only
-- database it was applied to (the development one) had the same account
-- everywhere, so it was corrected in place rather than followed by another.
--
-- A company whose manufacturing inventory account differs from the one already
-- chosen for it must not be switched silently: its stock would go on
-- posting, to another account, and the old balance would stay behind. The
-- upgrade stops and says what to do. Nothing of this migration is applied
-- then, and the upgrade can be run again.
DO $$
DECLARE
    clash record;
BEGIN
    SELECT c.code AS company, mine.code AS mine, theirs.code AS theirs INTO clash
    FROM manufacturing_settings s
    JOIN account_rule r ON r.company_id = s.company_id AND r.purpose = 'inventory'
                       AND r.scope_type IS NULL
    JOIN company c ON c.id = s.company_id
    JOIN account mine ON mine.id = s.materials_account_id
    JOIN account theirs ON theirs.id = r.account_id
    WHERE r.account_id <> s.materials_account_id
    ORDER BY c.code
    LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'company %: manufacturing takes components from materials account %, but the inventory account of the company is %. Upgrading would silently switch it to the other account and leave the balance behind. Before upgrading, make the two agree and move the balance with a journal entry; then upgrade again.',
            clash.company, clash.mine, clash.theirs;
    END IF;
END $$;

-- Only a rule for the same account can be in the way by now.
INSERT INTO account_rule (id, company_id, purpose, account_id)
SELECT gen_random_uuid(), company_id, 'inventory', materials_account_id FROM manufacturing_settings
ON CONFLICT DO NOTHING;

-- The same care for finished goods: a product that already has a rule of
-- its own must have it for the finished goods account.
DO $$
DECLARE
    clash record;
BEGIN
    SELECT c.code AS company, i.code AS item, mine.code AS mine, theirs.code AS theirs INTO clash
    FROM manufacturing_settings s
    CROSS JOIN (SELECT DISTINCT item_id FROM bom) p
    JOIN account_rule r ON r.company_id = s.company_id AND r.purpose = 'inventory'
                       AND r.scope_type = 'item' AND r.scope_id = p.item_id
    JOIN company c ON c.id = s.company_id
    JOIN item i ON i.id = p.item_id
    JOIN account mine ON mine.id = s.finished_goods_account_id
    JOIN account theirs ON theirs.id = r.account_id
    WHERE r.account_id <> s.finished_goods_account_id
    ORDER BY c.code, i.code
    LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'company %: manufacturing posts product % to finished goods account %, but the account rule for that item names account %. Upgrading would silently switch it to the other account and leave the balance behind. Before upgrading, make the two agree and move the balance with a journal entry; then upgrade again.',
            clash.company, clash.item, clash.mine, clash.theirs;
    END IF;
END $$;

INSERT INTO account_rule (id, company_id, purpose, scope_type, scope_id, account_id)
SELECT gen_random_uuid(), s.company_id, 'inventory', 'item', p.item_id, s.finished_goods_account_id
FROM manufacturing_settings s
JOIN account_rule d ON d.company_id = s.company_id AND d.purpose = 'inventory'
                   AND d.scope_type IS NULL
CROSS JOIN (SELECT DISTINCT item_id FROM bom) p
WHERE s.finished_goods_account_id <> d.account_id
ON CONFLICT DO NOTHING;

DROP TABLE manufacturing_settings;
DROP FUNCTION manufacturing_settings_check();
