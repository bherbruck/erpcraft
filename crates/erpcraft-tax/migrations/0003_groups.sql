-- More than one tax on a line. A tax code may be a group of component
-- codes: a sale in British Columbia carries GST and PST, a sale in a US
-- city carries state, county and city tax. Each component is a code of its
-- own, with its own rate and, per company, its own accounts, so each is
-- owed to and reported for its own authority. A code without components is
-- what it always was: one rate, one pair of accounts.
--
-- A component can be compounding: its tax is worked out on the net amount
-- plus the taxes of the components before it in the group (as Quebec QST
-- was until 2012). Otherwise it is on the net amount alone.
--
-- Tax is still worked out and rounded per line, now per component. Each
-- component's tax is one tax entry, so a return totals each authority's
-- tax by its code, and a credit takes back each component exactly.

ALTER TABLE tax_code
    -- Within a group: worked out on the net amount plus the taxes of the
    -- components before it.
    ADD COLUMN compound boolean NOT NULL DEFAULT false,
    -- Who the tax is owed to, for people: "Canada Revenue Agency".
    ADD COLUMN authority text;

CREATE TABLE tax_code_component (
    id uuid PRIMARY KEY,
    -- The code a line carries.
    group_id uuid NOT NULL REFERENCES tax_code (id),
    -- One of the taxes it is made of.
    component_id uuid NOT NULL REFERENCES tax_code (id),
    -- The order the taxes are worked out in: what a compounding component
    -- is charged on.
    position integer NOT NULL CHECK (position > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (group_id, position),
    UNIQUE (group_id, component_id),
    CHECK (group_id <> component_id)
);
CREATE INDEX tax_code_component_component ON tax_code_component (component_id);

-- Groups are one level deep: a group's components are plain codes.
CREATE FUNCTION tax_code_component_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM tax_code_component WHERE group_id = NEW.component_id) THEN
        RAISE EXCEPTION 'a tax group cannot be a component of another group'
            USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (SELECT 1 FROM tax_code_component WHERE component_id = NEW.group_id) THEN
        RAISE EXCEPTION 'a component of a tax group cannot itself have components'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tax_code_component_check BEFORE INSERT OR UPDATE ON tax_code_component
    FOR EACH ROW EXECUTE FUNCTION tax_code_component_check();

ALTER TABLE tax_entry
    -- The code the line carried, when it was a group and this entry is
    -- one of its components.
    ADD COLUMN group_code_id uuid REFERENCES tax_code (id),
    -- The component was compounding: base is the net amount plus the taxes
    -- of the components before it.
    ADD COLUMN compound boolean NOT NULL DEFAULT false;

CREATE OR REPLACE FUNCTION tax_entry_before_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    pst posting%ROWTYPE;
    orig tax_entry%ROWTYPE;
    credited tax_entry%ROWTYPE;
    taken_base numeric;
    taken_amount numeric;
BEGIN
    SELECT * INTO pst FROM posting WHERE id = NEW.posting_id;
    IF pst.company_id <> NEW.company_id OR pst.posting_date <> NEW.posting_date THEN
        RAISE EXCEPTION 'a tax entry must carry the company and date of its posting'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.reverses_id IS NOT NULL THEN
        SELECT * INTO orig FROM tax_entry WHERE id = NEW.reverses_id;
        IF orig.reverses_id IS NOT NULL THEN
            RAISE EXCEPTION 'a reversal cannot itself be reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.tax_code_id := orig.tax_code_id;
        NEW.direction := orig.direction;
        NEW.rate := orig.rate;
        NEW.base := -orig.base;
        NEW.amount := -orig.amount;
        -- Reversing a credit gives back what it had taken.
        NEW.credits_id := orig.credits_id;
        NEW.group_code_id := orig.group_code_id;
        NEW.compound := orig.compound;
    ELSIF NEW.credits_id IS NOT NULL THEN
        -- The entry credited, locked so that two credits of it take turns.
        SELECT * INTO credited FROM tax_entry WHERE id = NEW.credits_id FOR UPDATE;
        IF credited.company_id <> NEW.company_id OR credited.reverses_id IS NOT NULL
           OR credited.credits_id IS NOT NULL OR credited.base <= 0 THEN
            RAISE EXCEPTION 'a credit takes back tax that an invoice recorded, in the same company'
                USING ERRCODE = 'check_violation';
        END IF;
        IF EXISTS (SELECT 1 FROM tax_entry WHERE reverses_id = credited.id) THEN
            RAISE EXCEPTION 'the tax this credit would take back was reversed'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.base >= 0 OR NEW.amount > 0 THEN
            RAISE EXCEPTION 'a credit is recorded as negative amounts'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.tax_code_id := credited.tax_code_id;
        NEW.direction := credited.direction;
        NEW.rate := credited.rate;
        NEW.group_code_id := credited.group_code_id;
        NEW.compound := credited.compound;
        SELECT COALESCE(sum(base), 0), COALESCE(sum(amount), 0) INTO taken_base, taken_amount
            FROM tax_entry WHERE credits_id = credited.id;
        IF credited.base + taken_base + NEW.base < 0
           OR credited.amount + taken_amount + NEW.amount < 0 THEN
            RAISE EXCEPTION 'more tax would be credited than the invoice charged'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.base < 0 OR NEW.amount < 0 THEN
        RAISE EXCEPTION 'tax is recorded as positive amounts; only a reversal or a credit is negative'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
