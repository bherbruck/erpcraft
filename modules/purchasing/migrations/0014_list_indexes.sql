-- Finding vendors by part of their name or code (ILIKE), sorting them by
-- name, and sorting purchase documents by vendor: see sales'
-- 0013_list_indexes, of which this is the mirror.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX vendor_name_trgm ON vendor USING gin (name gin_trgm_ops);
CREATE INDEX vendor_code_trgm ON vendor USING gin (code gin_trgm_ops);
CREATE INDEX vendor_name ON vendor (name);
CREATE INDEX document_vendor_name ON document (doc_type, company_id, (summary->>'vendor_name'));
