-- EDIFACT partners (PLAN.md, phase 3): what an interchange to them is
-- written in. The separators are EDIFACT's own and stated in the UNA that
-- is always written; the decimal mark and the character set are the
-- partner's choice. An X12 partner keeps X12's versions and
-- acknowledgements, an EDIFACT partner D.96A and CONTRL (or none).
ALTER TABLE edi_partner
    ADD COLUMN decimal_mark text NOT NULL DEFAULT '.' CHECK (decimal_mark IN ('.', ',')),
    ADD COLUMN charset text NOT NULL DEFAULT 'UNOC'
        CHECK (charset IN ('UNOA', 'UNOB', 'UNOC', 'UNOY')),
    ADD CONSTRAINT edi_partner_syntax_version CHECK (
        (syntax = 'edifact') = (version = 'D96A')
        AND (syntax = 'edifact' OR ack_form IN ('997', '999', 'none'))
        AND (syntax = 'x12' OR ack_form IN ('contrl', 'none'))) NOT VALID;
