-- Which definition of its type's summary a document's summary was worked
-- out by. A module bumps its type's summary version when the figures it
-- keeps change; documents whose summary is of an older version are worked
-- out again, as those with none are.
ALTER TABLE document ADD COLUMN summary_version integer NOT NULL DEFAULT 0;
