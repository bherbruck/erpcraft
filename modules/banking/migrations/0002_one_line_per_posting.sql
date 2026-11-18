-- One movement of money explains one bank line: a posting is reconciled to
-- at most one statement line, across all statements.
DROP INDEX bank_statement_line_posting;
CREATE UNIQUE INDEX bank_statement_line_posting ON bank_statement_line (posting_id)
    WHERE posting_id IS NOT NULL;
