-- Tax rules: which tax code an invoice line starts with when it names
-- none, per company. A rule for a customer or vendor (an exempt buyer,
-- a buyer in another member state under the reverse charge) comes first;
-- then one for the line's item; then the charge's own code; then the
-- company's default, the rule without a scope. Like account rules, the
-- tax module does not know what a scope is: it only matches it.
CREATE TABLE tax_rule (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    scope_type text CHECK (scope_type IN ('customer', 'vendor', 'item')),
    scope_id uuid,
    tax_code_id uuid NOT NULL REFERENCES tax_code (id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tax_rule_scope_whole CHECK ((scope_type IS NULL) = (scope_id IS NULL)),
    CONSTRAINT tax_rule_once UNIQUE NULLS NOT DISTINCT (company_id, scope_type, scope_id)
);
CREATE INDEX tax_rule_scope ON tax_rule (scope_type, scope_id);
