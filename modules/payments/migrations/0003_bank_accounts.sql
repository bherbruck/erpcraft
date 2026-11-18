-- Bank and cash accounts (account.is_bank, from the general ledger).
--
-- Every account a posted (or posted and then reversed) payment went
-- through is a bank or cash account. Drafts say nothing yet.
UPDATE account a SET is_bank = true
WHERE EXISTS (
    SELECT 1 FROM payment p
    JOIN posting po ON po.document_id = p.document_id
    WHERE p.bank_account_id = a.id
);

-- A payables or receivables control account is never a bank account: a
-- payment through it would debit and credit the one account, and open
-- items would no longer add up to it. Checked both ways: an account
-- marked as a bank account that is a control account, and a control
-- account named in the settings that is marked as a bank account.
-- The settings belong to purchasing and sales, which may not be
-- installed, so they are looked up only where they exist.
CREATE FUNCTION account_control_use(account uuid) RETURNS text
LANGUAGE plpgsql STABLE AS $$
DECLARE
    found boolean;
BEGIN
    IF EXISTS (SELECT 1 FROM open_item WHERE account_id = account) THEN
        RETURN 'the control account of open items';
    END IF;
    IF to_regclass('purchasing_settings') IS NOT NULL THEN
        EXECUTE 'SELECT EXISTS (SELECT 1 FROM purchasing_settings WHERE payable_account_id = $1)'
            INTO found USING account;
        IF found THEN
            RETURN 'the payables account in the purchasing settings';
        END IF;
    END IF;
    IF to_regclass('sales_settings') IS NOT NULL THEN
        EXECUTE 'SELECT EXISTS (SELECT 1 FROM sales_settings WHERE receivable_account_id = $1)'
            INTO found USING account;
        IF found THEN
            RETURN 'the receivables account in the sales settings';
        END IF;
    END IF;
    RETURN NULL;
END $$;

CREATE FUNCTION account_bank_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    used text;
BEGIN
    used := account_control_use(NEW.id);
    IF used IS NOT NULL THEN
        RAISE EXCEPTION 'account % is %, so it cannot be a bank or cash account', NEW.code, used
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER account_bank_check BEFORE INSERT OR UPDATE OF is_bank ON account
    FOR EACH ROW WHEN (NEW.is_bank) EXECUTE FUNCTION account_bank_check();

-- The other way: naming a bank or cash account as a control account.
CREATE FUNCTION control_account_not_bank() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    control uuid;
    code text;
BEGIN
    control := CASE TG_TABLE_NAME
        WHEN 'purchasing_settings' THEN (to_jsonb(NEW) ->> 'payable_account_id')::uuid
        WHEN 'sales_settings' THEN (to_jsonb(NEW) ->> 'receivable_account_id')::uuid
        ELSE (to_jsonb(NEW) ->> 'account_id')::uuid
    END;
    SELECT a.code INTO code FROM account a WHERE a.id = control AND a.is_bank;
    IF code IS NOT NULL THEN
        RAISE EXCEPTION 'account % is a bank or cash account, so it cannot be a control account of payables or receivables', code
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER open_item_control_not_bank BEFORE INSERT ON open_item
    FOR EACH ROW EXECUTE FUNCTION control_account_not_bank();

DO $$
BEGIN
    IF to_regclass('purchasing_settings') IS NOT NULL THEN
        CREATE TRIGGER purchasing_settings_control_not_bank
            BEFORE INSERT OR UPDATE ON purchasing_settings
            FOR EACH ROW EXECUTE FUNCTION control_account_not_bank();
    END IF;
    IF to_regclass('sales_settings') IS NOT NULL THEN
        CREATE TRIGGER sales_settings_control_not_bank
            BEFORE INSERT OR UPDATE ON sales_settings
            FOR EACH ROW EXECUTE FUNCTION control_account_not_bank();
    END IF;
END $$;
