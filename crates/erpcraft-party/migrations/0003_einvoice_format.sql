-- How a customer receives its invoices when they are sent by email: a PDF
-- (pdf), a ZUGFeRD / Factur-X PDF with the invoice inside (zugferd), or the
-- XRechnung XML alone, in UBL or CII (xrechnung_ubl, xrechnung_cii).
-- Null: worked out from what the customer has on record (a Leitweg-ID
-- means XRechnung).
ALTER TABLE party_profile ADD COLUMN einvoice_format text
    CHECK (einvoice_format IN ('pdf', 'zugferd', 'xrechnung_ubl', 'xrechnung_cii'));
ALTER TABLE party_profile ADD CONSTRAINT party_profile_einvoice_format_customer
    CHECK (einvoice_format IS NULL OR party_type = 'customer');
