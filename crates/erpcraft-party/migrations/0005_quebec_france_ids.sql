-- Two more tax identifiers a printed invoice names: the Quebec sales tax
-- (TVQ/QST) registration number, which Revenu Québec issues apart from
-- the federal business number that carries the GST/HST account
-- (1234567890TQ0001); and the French SIREN, the nine digits that identify
-- a business in France and that a French invoice states for the seller
-- and, from the e-invoicing reform on, for the buyer.
ALTER TABLE party_tax_id DROP CONSTRAINT party_tax_id_scheme_check;
ALTER TABLE party_tax_id ADD CONSTRAINT party_tax_id_scheme_check
    CHECK (scheme IN ('eu_vat', 'tax_number', 'us_ein', 'ca_bn', 'ca_qst', 'fr_siren', 'other'));
ALTER TABLE party_tax_id ADD CONSTRAINT party_tax_id_siren_check
    CHECK (scheme <> 'fr_siren' OR regexp_replace(value, '\s', '', 'g') ~ '^[0-9]{9}$');
