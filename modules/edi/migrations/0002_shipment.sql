-- How deliveries to EDI partners were packed: the shipment's carrier and
-- papers, its pallets and cartons with their SSCCs, and which delivery
-- line's goods are in which carton. The ship notice (856) is made from
-- this and the posted delivery, never typed.

-- Per company: the GS1 company prefix SSCCs are made from, and the next
-- serial. The row is locked while a serial is taken, so no two cartons
-- get one serial; the serial never goes back.
CREATE TABLE edi_settings (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL UNIQUE REFERENCES company (id),
    gs1_company_prefix text CHECK (gs1_company_prefix ~ '^[0-9]{7,10}$'),
    sscc_extension int NOT NULL DEFAULT 0 CHECK (sscc_extension BETWEEN 0 AND 9),
    next_sscc_serial bigint NOT NULL DEFAULT 1 CHECK (next_sscc_serial >= 1),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION edi_settings_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.next_sscc_serial < OLD.next_sscc_serial THEN
        RAISE EXCEPTION 'the next SSCC serial never goes back: a serial is used once'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_settings_guard BEFORE UPDATE ON edi_settings
    FOR EACH ROW EXECUTE FUNCTION edi_settings_guard();

-- One per delivery to an EDI partner.
CREATE TABLE edi_shipment (
    -- A sales delivery.
    delivery_id uuid PRIMARY KEY REFERENCES document (id),
    company_id uuid NOT NULL REFERENCES company (id),
    partner_id uuid NOT NULL REFERENCES edi_partner (id),
    -- TD503: the carrier's SCAC.
    carrier_scac text CHECK (carrier_scac ~ '^[A-Z0-9]{2,4}$'),
    carrier_name text CHECK (carrier_name <> ''),
    service text CHECK (service <> ''),
    bill_of_lading text CHECK (bill_of_lading <> ''),
    -- The carrier's PRO or tracking number.
    tracking text CHECK (tracking <> ''),
    shipped_at timestamptz,
    estimated_delivery date,
    gross_weight numeric(20, 6) CHECK (gross_weight > 0),
    weight_uom text CHECK (weight_uom ~ '^[A-Z]{2}$'),
    -- packed: the cartons hold exactly what the delivery delivers.
    state text NOT NULL DEFAULT 'packing' CHECK (state IN ('packing', 'packed')),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- A pallet or a carton. Never deleted: repacking voids the packs, and an
-- SSCC once issued names that pack for good.
CREATE TABLE edi_pack (
    id uuid PRIMARY KEY,
    delivery_id uuid NOT NULL REFERENCES edi_shipment (delivery_id),
    -- The pallet a carton is on.
    parent_id uuid REFERENCES edi_pack (id),
    kind text NOT NULL CHECK (kind IN ('pallet', 'carton')),
    sscc text NOT NULL UNIQUE CHECK (sscc ~ '^[0-9]{18}$' AND edi_gs1_valid(sscc)),
    weight numeric(20, 6) CHECK (weight > 0),
    -- Replaced by a later packing of the same delivery.
    voided boolean NOT NULL DEFAULT false,
    -- The order packs were given in, from 1.
    position int NOT NULL CHECK (position >= 1),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (kind = 'carton' OR parent_id IS NULL)
);
CREATE INDEX edi_pack_delivery ON edi_pack (delivery_id) WHERE NOT voided;

CREATE FUNCTION edi_pack_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'a pack is never deleted: its SSCC was issued'
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.sscc <> OLD.sscc OR NEW.delivery_id <> OLD.delivery_id
                              OR NEW.kind <> OLD.kind OR (OLD.voided AND NOT NEW.voided)) THEN
        RAISE EXCEPTION 'a pack keeps its SSCC, delivery and kind, and stays void once voided'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.parent_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM edi_pack p WHERE p.id = NEW.parent_id AND p.kind = 'pallet'
                                   AND p.delivery_id = NEW.delivery_id
    ) THEN
        RAISE EXCEPTION 'a carton stands on a pallet of the same delivery'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_pack_guard BEFORE INSERT OR UPDATE OR DELETE ON edi_pack
    FOR EACH ROW EXECUTE FUNCTION edi_pack_guard();

-- What is in a carton: a quantity of a delivery line, in the item's unit.
CREATE TABLE edi_pack_line (
    pack_id uuid NOT NULL REFERENCES edi_pack (id),
    delivery_line_id uuid NOT NULL,
    qty numeric(20, 6) NOT NULL CHECK (qty > 0),
    PRIMARY KEY (pack_id, delivery_line_id)
);

CREATE FUNCTION edi_pack_line_check() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM edi_pack p JOIN sales_delivery_line l ON l.document_id = p.delivery_id
        WHERE p.id = NEW.pack_id AND l.id = NEW.delivery_line_id AND p.kind = 'carton'
    ) THEN
        RAISE EXCEPTION 'goods are packed in a carton, from a line of the carton''s delivery'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER edi_pack_line_check BEFORE INSERT OR UPDATE ON edi_pack_line
    FOR EACH ROW EXECUTE FUNCTION edi_pack_line_check();

-- When a shipment is packed, at commit: the cartons in use hold, per
-- delivery line, exactly what the line delivers, and none is empty.
CREATE FUNCTION edi_shipment_packed_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    delivery uuid;
BEGIN
    IF TG_TABLE_NAME = 'edi_shipment' THEN
        delivery := NEW.delivery_id;
    ELSIF TG_TABLE_NAME = 'edi_pack' THEN
        delivery := COALESCE(NEW.delivery_id, OLD.delivery_id);
    ELSE
        SELECT p.delivery_id INTO delivery FROM edi_pack p
        WHERE p.id = COALESCE(NEW.pack_id, OLD.pack_id);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM edi_shipment WHERE delivery_id = delivery AND state = 'packed') THEN
        RETURN NULL;
    END IF;
    IF EXISTS (
        SELECT 1 FROM edi_pack p
        WHERE p.delivery_id = delivery AND NOT p.voided AND p.kind = 'carton'
          AND NOT EXISTS (SELECT 1 FROM edi_pack_line pl WHERE pl.pack_id = p.id)
    ) THEN
        RAISE EXCEPTION 'a packed shipment has no empty carton' USING ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM sales_delivery_line l
        LEFT JOIN (SELECT pl.delivery_line_id, sum(pl.qty) AS qty
                   FROM edi_pack_line pl JOIN edi_pack p ON p.id = pl.pack_id
                   WHERE p.delivery_id = delivery AND NOT p.voided
                   GROUP BY pl.delivery_line_id) packed ON packed.delivery_line_id = l.id
        WHERE l.document_id = delivery AND COALESCE(packed.qty, 0) <> l.qty
    ) THEN
        RAISE EXCEPTION 'the cartons of a packed shipment hold exactly what each delivery line delivers'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER edi_shipment_packed AFTER INSERT OR UPDATE ON edi_shipment
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION edi_shipment_packed_check();
CREATE CONSTRAINT TRIGGER edi_pack_packed AFTER INSERT OR UPDATE ON edi_pack
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION edi_shipment_packed_check();
CREATE CONSTRAINT TRIGGER edi_pack_line_packed AFTER INSERT OR UPDATE OR DELETE ON edi_pack_line
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION edi_shipment_packed_check();
