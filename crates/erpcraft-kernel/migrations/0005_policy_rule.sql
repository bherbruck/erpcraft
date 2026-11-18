-- Policy rules: how a company has configured the business judgements that
-- modules declare as policy points.
--
-- The rules of a point are looked at in order of priority, lowest first,
-- a company's own before those for all companies at the same priority.
-- The first whose condition holds decides; if none does, the point's
-- default decides.
CREATE TABLE policy_rule (
    id uuid PRIMARY KEY,
    -- NULL: the rule applies to every company.
    company_id uuid REFERENCES company (id),
    -- The name of a policy point a module declares, such as
    -- 'purchase_receipt.over_receipt'.
    point text NOT NULL CHECK (point <> ''),
    effect text NOT NULL CHECK (effect IN ('allow', 'refuse')),
    -- An expression over facts and user. NULL: the rule always applies.
    condition text CHECK (condition <> ''),
    -- What the user is told when the rule refuses. {fact} shows a fact.
    message text,
    priority integer NOT NULL DEFAULT 100,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX policy_rule_point ON policy_rule (point, priority) WHERE active;
