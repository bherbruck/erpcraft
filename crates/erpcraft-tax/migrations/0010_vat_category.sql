-- The VAT category of EN 16931 a tax code stands for (UNTDID 5305 as the
-- standard restricts it), with the reason a sale is not taxed. It is what
-- an electronic invoice writes for a line (BT-151) and its VAT breakdown
-- (BT-118, BT-120, BT-121), and what the codes of an incoming invoice are
-- matched by. It belongs to the code because it is what the code means:
-- a sale under the code is standard rated, zero rated, exempt, under the
-- domestic reverse charge, an intra-Community supply or an export.
--
--   S   standard rate                    Z   zero rated
--   E   exempt from VAT                  AE  reverse charge (VATEX-EU-AE)
--   K   intra-Community supply           G   export outside the EU
--       (VATEX-EU-IC)                        (VATEX-EU-G)
--   O   not subject to VAT (VATEX-EU-O)  L   Canary Islands (IGIC)
--   M   Ceuta and Melilla (IPSI)
--
-- Empty means not stated: a code above 0 % that the buyer does not
-- account for is then standard rated; any other code must say, and an
-- e-invoice with a line under it is refused until it does.
ALTER TABLE tax_code
    ADD COLUMN vat_category text
        CHECK (vat_category IN ('S', 'Z', 'E', 'AE', 'K', 'G', 'O', 'L', 'M')),
    -- BT-121: a code of the VATEX list, such as VATEX-EU-IC.
    ADD COLUMN exemption_reason_code text CHECK (exemption_reason_code <> ''),
    -- BT-120: the reason in words, such as "Steuerfreie innergemeinschaftliche
    -- Lieferung (§ 4 Nr. 1b UStG)".
    ADD COLUMN exemption_reason text CHECK (exemption_reason <> ''),
    -- Only a sale that is not taxed has a reason: the standard forbids one
    -- for S and Z (BR-S-10, BR-Z-10) and for L and M.
    ADD CONSTRAINT tax_code_exemption_reason CHECK (
        (exemption_reason IS NULL AND exemption_reason_code IS NULL)
        OR vat_category IN ('E', 'AE', 'K', 'G', 'O'));

-- The categories of the codes there are, from what each is today. A group
-- (a code made of components) has none: EN 16931 has one rate per line,
-- and an e-invoice refuses a group.
--
-- 1. Tax the buyer accounts for (self_assessed): the domestic reverse
--    charge, AE, with VATEX-EU-AE. Until now every such code was written
--    as AE on an e-invoice, so this keeps what the codes did; a code that
--    stands for an intra-Community supply or an export is set to K or G
--    by hand.
UPDATE tax_code c
SET vat_category = 'AE', exemption_reason_code = 'VATEX-EU-AE',
    exemption_reason = 'Reverse charge'
WHERE c.self_assessed
  AND NOT EXISTS (SELECT 1 FROM tax_code_component g WHERE g.group_id = c.id);

-- 2. A rate above zero the seller charges: standard rated, S.
UPDATE tax_code c
SET vat_category = 'S'
WHERE NOT c.self_assessed AND c.rate > 0
  AND NOT EXISTS (SELECT 1 FROM tax_code_component g WHERE g.group_id = c.id);

-- 3. A rate of zero whose code or name says it is exempt ("exempt",
--    "steuerfrei", "befreit", as the Canadian template's CA-EXEMPT): E,
--    with the code's name as the reason, which the standard requires
--    (BR-E-10).
UPDATE tax_code c
SET vat_category = 'E', exemption_reason = c.name
WHERE NOT c.self_assessed AND c.rate = 0
  AND (c.code || ' ' || c.name) ~* '(exempt|steuerfrei|befreit)'
  AND NOT EXISTS (SELECT 1 FROM tax_code_component g WHERE g.group_id = c.id);

-- 4. Any other rate of zero: zero rated, Z.
UPDATE tax_code c
SET vat_category = 'Z'
WHERE NOT c.self_assessed AND c.rate = 0 AND c.vat_category IS NULL
  AND NOT EXISTS (SELECT 1 FROM tax_code_component g WHERE g.group_id = c.id);
