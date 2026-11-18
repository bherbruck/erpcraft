-- Delegation of approvals: while someone is away, the person they name
-- decides the requests they could decide, on their behalf, from one day
-- to another. A decision on someone's behalf says so, and counts as
-- theirs: the same person is never counted twice, in person and by proxy.
CREATE TABLE approval_delegation (
    id uuid PRIMARY KEY,
    -- Whose approvals are delegated.
    user_id uuid NOT NULL REFERENCES app_user (id),
    -- Who decides in their place.
    delegate_id uuid NOT NULL REFERENCES app_user (id),
    starts_on date NOT NULL,
    ends_on date NOT NULL,
    reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (user_id <> delegate_id),
    CHECK (ends_on >= starts_on)
);
CREATE INDEX approval_delegation_delegate ON approval_delegation (delegate_id, ends_on);

ALTER TABLE approval_decision ADD COLUMN on_behalf_of uuid REFERENCES app_user (id);
ALTER TABLE approval_decision ADD CONSTRAINT approval_decision_not_for_oneself
    CHECK (on_behalf_of IS NULL OR on_behalf_of <> user_id);
-- One decision per person whose right it is, whoever made it.
CREATE UNIQUE INDEX approval_decision_one_per_authority
    ON approval_decision (request_id, COALESCE(on_behalf_of, user_id));
