-- The tax numbers of people, which information returns name: a US
-- Social Security Number (or ITIN) of an individual paid for services, and
-- a Canadian Social Insurance Number of a subcontractor without a business
-- number. They are read by whoever may read the party; an information
-- return shows them whole only to who may export it.
ALTER TABLE party_tax_id DROP CONSTRAINT party_tax_id_scheme_check;
ALTER TABLE party_tax_id ADD CONSTRAINT party_tax_id_scheme_check
    CHECK (scheme IN ('eu_vat', 'tax_number', 'us_ein', 'us_ssn', 'ca_bn', 'ca_qst', 'ca_sin',
                      'fr_siren', 'other'));
ALTER TABLE party_tax_id ADD CONSTRAINT party_tax_id_ssn_check
    CHECK (scheme <> 'us_ssn' OR regexp_replace(value, '[\s-]', '', 'g') ~ '^[0-9]{9}$');
ALTER TABLE party_tax_id ADD CONSTRAINT party_tax_id_sin_check
    CHECK (scheme <> 'ca_sin' OR regexp_replace(value, '[\s-]', '', 'g') ~ '^[0-9]{9}$');
