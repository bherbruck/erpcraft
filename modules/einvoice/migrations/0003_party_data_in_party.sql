-- Addresses, tax identifiers, electronic addresses and contacts are party
-- data, kept by the party module for the company, customers and vendors
-- alike; printing and tax reports read them too. What this module kept in
-- einvoice_profile and einvoice_party moves there. Where the party module
-- has a record of its own already, that record stays and nothing is added
-- beside it. Then this module's two tables go.

-- The company's particulars.
INSERT INTO party_profile (id, party_type, party_id, legal_name, trading_name,
                           legal_registration, legal_info, payment_account_id)
SELECT id, 'company', company_id, legal_name, trading_name, legal_registration, legal_info,
       payment_account_id
FROM einvoice_profile
ON CONFLICT (party_type, party_id) DO NOTHING;

-- A customer's or vendor's particulars, and a customer's invoice reference.
INSERT INTO party_profile (id, party_type, party_id, legal_name, legal_registration,
                           invoice_reference)
SELECT id, party_type, party_id, legal_name, legal_registration,
       CASE WHEN party_type = 'customer' THEN buyer_reference END
FROM einvoice_party
WHERE num_nonnulls(legal_name, legal_registration,
                   CASE WHEN party_type = 'customer' THEN buyer_reference END) > 0
ON CONFLICT (party_type, party_id) DO NOTHING;

-- Everything else, for both tables alike.
CREATE TEMPORARY TABLE einvoice_moving AS
SELECT 'company'::text AS party_type, company_id AS party_id, vat_id, tax_number, street,
       additional_street, city, postcode, country_code, electronic_address,
       electronic_address_scheme, contact_name, contact_phone, contact_email
FROM einvoice_profile
UNION ALL
SELECT party_type, party_id, vat_id, tax_number, street, additional_street, city, postcode,
       country_code, electronic_address, electronic_address_scheme, contact_name,
       contact_phone, contact_email
FROM einvoice_party;

-- The billing address, the default unless the party has one.
INSERT INTO party_address (id, party_type, party_id, kind, is_default, street,
                           additional_street, city, postcode, country_code)
SELECT gen_random_uuid(), m.party_type, m.party_id, 'billing',
       NOT EXISTS (SELECT 1 FROM party_address a WHERE a.party_type = m.party_type
                   AND a.party_id = m.party_id AND a.kind = 'billing' AND a.is_default),
       m.street, m.additional_street, m.city, m.postcode, m.country_code
FROM einvoice_moving m
WHERE m.country_code IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM party_address a WHERE a.party_type = m.party_type
                  AND a.party_id = m.party_id AND a.kind = 'billing');

-- The VAT identifier and the tax number. A VAT identifier starts with its
-- country (EL for Greece).
INSERT INTO party_tax_id (id, party_type, party_id, scheme, value, country_code)
SELECT gen_random_uuid(), party_type, party_id, 'eu_vat', vat_id,
       CASE WHEN upper(left(vat_id, 2)) ~ '^[A-Z]{2}$' THEN upper(left(vat_id, 2)) END
FROM einvoice_moving WHERE vat_id IS NOT NULL
ON CONFLICT (party_type, party_id, scheme) DO NOTHING;
INSERT INTO party_tax_id (id, party_type, party_id, scheme, value, country_code)
SELECT gen_random_uuid(), party_type, party_id, 'tax_number', tax_number, country_code
FROM einvoice_moving WHERE tax_number IS NOT NULL
ON CONFLICT (party_type, party_id, scheme) DO NOTHING;

-- The electronic address, the default unless the party has one.
INSERT INTO party_electronic_address (id, party_type, party_id, scheme, value, is_default)
SELECT gen_random_uuid(), m.party_type, m.party_id, m.electronic_address_scheme,
       m.electronic_address,
       NOT EXISTS (SELECT 1 FROM party_electronic_address e WHERE e.party_type = m.party_type
                   AND e.party_id = m.party_id AND e.is_default)
FROM einvoice_moving m WHERE m.electronic_address IS NOT NULL
ON CONFLICT (party_type, party_id, scheme, value) DO NOTHING;

-- The contact, the default unless the party has one.
INSERT INTO party_contact (id, party_type, party_id, name, phone, email, is_default)
SELECT gen_random_uuid(), m.party_type, m.party_id, m.contact_name, m.contact_phone,
       m.contact_email,
       NOT EXISTS (SELECT 1 FROM party_contact c WHERE c.party_type = m.party_type
                   AND c.party_id = m.party_id AND c.is_default)
FROM einvoice_moving m
WHERE num_nonnulls(m.contact_name, m.contact_phone, m.contact_email) > 0
  AND NOT EXISTS (SELECT 1 FROM party_contact c WHERE c.party_type = m.party_type
                  AND c.party_id = m.party_id);

DROP TABLE einvoice_moving;
DROP TABLE einvoice_profile;
DROP TABLE einvoice_party;
DROP FUNCTION einvoice_profile_check();
DROP FUNCTION einvoice_party_check();
DROP FUNCTION einvoice_bank_check(uuid, uuid);
