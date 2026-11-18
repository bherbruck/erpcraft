-- What an approval is bound to, beyond the facts of the point that asked.
--
-- needs: every point that asked for approval while the command was tried,
-- with the facts it asked on: [{point, rule_id, facts}]. One approval
-- grants them all; the run is allowed at each point only for its facts.
-- Requests from before this hold their one point in point and facts.
--
-- target_id, target_version: the document the command acts on and its
-- version when approval was asked. If the document changed since (another
-- vendor, another bank account, at the same total), the request is stale
-- and does not run.
--
-- scopes: the scopes of the credential the requester asked with. The
-- approved run is capped by them as the request was.
ALTER TABLE approval_request
    ADD COLUMN needs jsonb,
    ADD COLUMN target_id uuid,
    ADD COLUMN target_version integer,
    ADD COLUMN scopes jsonb,
    ADD CONSTRAINT approval_request_target_whole
        CHECK ((target_id IS NULL) = (target_version IS NULL));
