-- EDI phase 7 (PLAN.md): inventory advice, order changes, remittances.
--
-- A partner may be a vendor's as well as (or instead of) a customer's:
-- remittance advice goes to the vendors a payment run pays, and comes from
-- the customers that pay. A partner trades for at least one of them.
ALTER TABLE edi_partner
    ALTER COLUMN customer_id DROP NOT NULL,
    ADD COLUMN vendor_id uuid REFERENCES vendor (id),
    -- Every how many minutes the partner is sent an inventory advice (846
    -- or INVRPT); none when it is sent only on demand.
    ADD COLUMN inventory_minutes int CHECK (inventory_minutes BETWEEN 15 AND 10080),
    -- Whether a posted payment run paying the partner's vendor sends it a
    -- remittance advice (820 or REMADV).
    ADD COLUMN remittance_out boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT edi_partner_party CHECK (customer_id IS NOT NULL OR vendor_id IS NOT NULL),
    ADD CONSTRAINT edi_partner_remittance CHECK (NOT remittance_out OR vendor_id IS NOT NULL),
    ADD CONSTRAINT edi_partner_inventory CHECK (inventory_minutes IS NULL OR customer_id IS NOT NULL);

-- When each partner was last sent an advice by the schedule.
ALTER TABLE edi_partner ADD COLUMN inventory_sent_at timestamptz;

CREATE INDEX edi_partner_vendor ON edi_partner (vendor_id) WHERE vendor_id IS NOT NULL;
