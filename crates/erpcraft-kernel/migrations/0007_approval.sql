-- Approvals: a third outcome of a policy rule. Where a rule with effect
-- 'approve' decides, the command does not run; it is kept as an approval
-- request, and runs as the person who asked once enough others approved.

ALTER TABLE policy_rule DROP CONSTRAINT policy_rule_effect_check;
ALTER TABLE policy_rule ADD CONSTRAINT policy_rule_effect_check
    CHECK (effect IN ('allow', 'refuse', 'approve'));
-- Who may approve: holders of this role (for the request's company or for
-- all companies), or holders of this permission there, or both.
ALTER TABLE policy_rule ADD COLUMN approver_role_id uuid REFERENCES role (id);
ALTER TABLE policy_rule ADD COLUMN approver_permission text CHECK (approver_permission <> '');
-- How many distinct people must approve, none of them the one who asked.
ALTER TABLE policy_rule ADD COLUMN approvals_needed integer NOT NULL DEFAULT 1
    CHECK (approvals_needed BETWEEN 1 AND 10);
-- How long a request waits before it expires.
ALTER TABLE policy_rule ADD COLUMN approval_days integer NOT NULL DEFAULT 14
    CHECK (approval_days BETWEEN 1 AND 365);
ALTER TABLE policy_rule ADD CONSTRAINT policy_rule_approvers CHECK (
    (effect = 'approve') = (approver_role_id IS NOT NULL OR approver_permission IS NOT NULL)
);

CREATE TABLE approval_request (
    id uuid PRIMARY KEY,
    company_id uuid NOT NULL REFERENCES company (id),
    -- The command, and exactly what it was given.
    command text NOT NULL,
    input jsonb NOT NULL,
    requested_by uuid NOT NULL REFERENCES app_user (id),
    requested_at timestamptz NOT NULL DEFAULT now(),
    -- The idempotency key the request came with, so that a retry finds it.
    idempotency_key text,
    -- What asked for approval: the point, the rule, and the facts it
    -- decided on, as conditions saw them and as a person reads them.
    point text NOT NULL,
    rule_id uuid REFERENCES policy_rule (id) ON DELETE SET NULL,
    facts jsonb NOT NULL,
    shown_facts jsonb NOT NULL,
    message text,
    approver_role_id uuid REFERENCES role (id),
    approver_permission text,
    approvals_needed integer NOT NULL CHECK (approvals_needed BETWEEN 1 AND 10),
    expires_at timestamptz NOT NULL,
    -- pending: waiting. executed: approved and run. failed: approved, and
    -- the run was refused. stale: approved, and what it was about had
    -- changed. rejected. An expired request stays pending past expires_at.
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'executed', 'failed', 'stale', 'rejected')),
    -- What the command returned when it ran, or why it did not.
    result jsonb,
    error text,
    decided_at timestamptz
);
CREATE INDEX approval_request_pending ON approval_request (company_id, requested_at)
    WHERE status = 'pending';
CREATE INDEX approval_request_requester ON approval_request (requested_by, requested_at);

-- One decision per person per request.
CREATE TABLE approval_decision (
    request_id uuid NOT NULL REFERENCES approval_request (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES app_user (id),
    decision text NOT NULL CHECK (decision IN ('approve', 'reject')),
    reason text,
    decided_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, user_id)
);
