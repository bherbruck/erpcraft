-- Lists of documents filtered by a date range (a grid's "this year"), per
-- type and company as every document list is.
CREATE INDEX document_type_company_date ON document (doc_type, company_id, doc_date);

-- Finding the documents whose summary is missing or was worked out by an
-- older version of their type's summary, without reading every document:
-- the server asks this at every start.
CREATE INDEX document_summary_version ON document (doc_type, summary_version);
CREATE INDEX document_summary_missing ON document (doc_type) WHERE summary = '{}'::jsonb;
