-- Finding customers by part of their name or code, as list filters
-- (`containsIgnoreCase`, `startsWithIgnoreCase`) and search (`q`) do:
-- ILIKE, which a trigram index serves.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX customer_name_trgm ON customer USING gin (name gin_trgm_ops);
CREATE INDEX customer_code_trgm ON customer USING gin (code gin_trgm_ops);
-- Sorting customers by name, and paging deep into that order.
CREATE INDEX customer_name ON customer (name);
-- Sales documents sorted by customer: a document list sorts a summary's
-- link by the name the summary copies, as exactly this expression (no
-- cast, the default collation), ascending with nulls last.
CREATE INDEX document_customer_name ON document (doc_type, company_id, (summary->>'customer_name'));
