-- The figures a document type keeps with the header of each of its
-- documents: who it is with, what it comes to. The type's module works
-- them out whenever the body is saved or the document changes state, so
-- that lists can show them and sort by them without reading a body. They
-- repeat what the body and the ledgers say and can always be worked out
-- again, so a locked document's summary may still change.
ALTER TABLE document ADD COLUMN summary jsonb NOT NULL DEFAULT '{}'
    CHECK (jsonb_typeof(summary) = 'object');
