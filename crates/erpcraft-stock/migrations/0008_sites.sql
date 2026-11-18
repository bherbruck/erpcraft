-- Sites and the location tree.
--
-- A site is a place of a company where it keeps goods: a plant, a
-- distribution center. A warehouse names its site and, optionally, the
-- location it is part of (`parent_id`): a site holds warehouses, a
-- warehouse holds locations or bins, each a warehouse row with its own
-- stock. A child is at its parent's site. A warehouse flagged `transit`
-- holds goods on their way between two warehouses.
--
-- Stock entries keep the site of their warehouse when they were recorded,
-- so that permission rules can read `record.site_id in user.sites`.

CREATE TABLE site (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    code text NOT NULL CHECK (code <> ''),
    name text NOT NULL CHECK (name <> ''),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (company_id, code),
    UNIQUE (id, company_id)
);

ALTER TABLE warehouse
    ADD COLUMN site_id uuid,
    ADD COLUMN parent_id uuid REFERENCES warehouse (id),
    ADD COLUMN transit boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT warehouse_site FOREIGN KEY (site_id, company_id) REFERENCES site (id, company_id);
CREATE INDEX warehouse_parent ON warehouse (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX warehouse_site ON warehouse (site_id) WHERE site_id IS NOT NULL;

-- A location is part of a warehouse of the same company and at its site;
-- a location without a site of its own takes its parent's. A location is
-- never inside itself, however far up. Changing a warehouse's site
-- changes its locations' with it.
CREATE FUNCTION stock_warehouse_tree() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    parent warehouse%ROWTYPE;
BEGIN
    IF NEW.parent_id IS NOT NULL THEN
        SELECT * INTO parent FROM warehouse WHERE id = NEW.parent_id;
        IF parent.company_id <> NEW.company_id THEN
            RAISE EXCEPTION 'a location must be part of a warehouse of the same company'
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.site_id IS NULL THEN
            NEW.site_id := parent.site_id;
        ELSIF NEW.site_id IS DISTINCT FROM parent.site_id THEN
            RAISE EXCEPTION 'a location is at the site of the warehouse it is part of'
                USING ERRCODE = 'check_violation';
        END IF;
        IF TG_OP = 'UPDATE' AND EXISTS (
            WITH RECURSIVE up AS (
                SELECT id, parent_id FROM warehouse WHERE id = NEW.parent_id
                UNION
                SELECT w.id, w.parent_id FROM warehouse w JOIN up ON w.id = up.parent_id
            )
            SELECT 1 FROM up WHERE id = NEW.id
        ) THEN
            RAISE EXCEPTION 'a location cannot be part of itself'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER warehouse_tree BEFORE INSERT OR UPDATE ON warehouse
    FOR EACH ROW EXECUTE FUNCTION stock_warehouse_tree();

CREATE FUNCTION stock_warehouse_site_down() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE warehouse SET site_id = NEW.site_id
        WHERE parent_id = NEW.id AND site_id IS DISTINCT FROM NEW.site_id;
    RETURN NULL;
END $$;
CREATE TRIGGER warehouse_site_down AFTER UPDATE OF site_id ON warehouse
    FOR EACH ROW WHEN (OLD.site_id IS DISTINCT FROM NEW.site_id)
    EXECUTE FUNCTION stock_warehouse_site_down();

ALTER TABLE stock_entry ADD COLUMN site_id uuid REFERENCES site (id);

CREATE FUNCTION stock_entry_site() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    SELECT site_id INTO NEW.site_id FROM warehouse WHERE id = NEW.warehouse_id;
    RETURN NEW;
END $$;
CREATE TRIGGER stock_entry_site BEFORE INSERT ON stock_entry
    FOR EACH ROW EXECUTE FUNCTION stock_entry_site();
