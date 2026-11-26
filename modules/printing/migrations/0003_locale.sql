-- A print template is for one language, or for any. A document prints in
-- its party's language: the default template of that language, else one
-- for any language, else the built-in one of that language.
ALTER TABLE print_template ADD COLUMN locale text CHECK (locale ~ '^[a-z]{2}$');
DROP INDEX print_template_default;
CREATE UNIQUE INDEX print_template_default
    ON print_template (doc_type, company_id, locale) NULLS NOT DISTINCT
    WHERE is_default;
