-- The VAT category of a tax code and its exemption reason are the tax
-- code's own now (tax migration 10: tax_code.vat_category,
-- exemption_reason_code, exemption_reason), so that what a code means is
-- said once, where the code is. What this module kept in
-- einvoice_tax_category moves there and replaces the category the tax
-- migration derived: it is what someone set for e-invoices. A reason is
-- kept only for the categories that may have one (E, AE, K, G, O); the
-- standard forbids one for the others. Then the table goes.
UPDATE tax_code c
SET vat_category = t.category,
    exemption_reason_code = CASE WHEN t.category IN ('E', 'AE', 'K', 'G', 'O')
                                 THEN t.exemption_reason_code END,
    exemption_reason = CASE WHEN t.category IN ('E', 'AE', 'K', 'G', 'O')
                            THEN t.exemption_reason END
FROM einvoice_tax_category t
WHERE t.tax_code_id = c.id;

DROP TABLE einvoice_tax_category;
