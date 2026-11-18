-- Webhooks: HTTP endpoints outside that are told about committed changes.

CREATE TABLE webhook (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (name <> ''),
    url text NOT NULL,
    -- Command names, or a prefix ending in `*`. Empty: every event.
    topics text[] NOT NULL DEFAULT '{}',
    -- Only events of this company. Null: events of every company, and
    -- those that belong to none.
    company_id uuid REFERENCES company (id),
    active boolean NOT NULL DEFAULT true,
    -- Why the last attempt failed. Cleared by the next one that succeeds.
    last_error text,
    last_status integer,
    failed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- The key each webhook's requests are signed with. It must be usable for
-- signing, so it is kept as given, not hashed. It is in a table of its
-- own, which is not an entity, so that no generic read can return it.
CREATE TABLE webhook_secret (
    webhook_id uuid PRIMARY KEY REFERENCES webhook (id) ON DELETE CASCADE,
    secret text NOT NULL CHECK (secret <> '')
);

-- One row per event and webhook: whether it arrived, and if not, why.
-- The outbox notes only that the subscriber as a whole is done, so this is
-- what keeps a webhook that has an event from being sent it again while
-- another one is failing. Rows go when their event is cleaned up.
CREATE TABLE webhook_delivery (
    event_id uuid NOT NULL REFERENCES outbox_event (id) ON DELETE CASCADE,
    webhook_id uuid NOT NULL REFERENCES webhook (id) ON DELETE CASCADE,
    topic text NOT NULL,
    attempts integer NOT NULL DEFAULT 1,
    -- What the endpoint answered at the last attempt, if it answered.
    status_code integer,
    error text,
    attempted_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz,
    PRIMARY KEY (event_id, webhook_id)
);
CREATE INDEX webhook_delivery_recent ON webhook_delivery (webhook_id, attempted_at DESC);
