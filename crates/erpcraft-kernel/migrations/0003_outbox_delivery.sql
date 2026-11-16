-- Outbox delivery: retries with backoff, and a record of which subscriber
-- has already handled which event.

ALTER TABLE outbox_event
    ADD COLUMN attempts integer NOT NULL DEFAULT 0,
    -- What the last failed delivery said.
    ADD COLUMN last_error text,
    ADD COLUMN next_attempt_at timestamptz NOT NULL DEFAULT now();

DROP INDEX outbox_event_unpublished;
CREATE INDEX outbox_event_due ON outbox_event (next_attempt_at) WHERE published_at IS NULL;
CREATE INDEX outbox_event_published ON outbox_event (published_at) WHERE published_at IS NOT NULL;

-- A subscriber that handled an event is not handed it again when delivery
-- to another subscriber fails and the event is retried.
CREATE TABLE outbox_delivery (
    event_id uuid NOT NULL REFERENCES outbox_event (id) ON DELETE CASCADE,
    subscriber text NOT NULL,
    delivered_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, subscriber)
);

CREATE INDEX idempotency_key_created ON idempotency_key (created_at);
