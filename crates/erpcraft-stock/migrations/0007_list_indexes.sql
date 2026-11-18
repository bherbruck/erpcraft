-- Finding items by part of their code or name, as list filters and search
-- do (ILIKE, served by trigram indexes).
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX item_name_trgm ON item USING gin (name gin_trgm_ops);
CREATE INDEX item_code_trgm ON item USING gin (code gin_trgm_ops);
