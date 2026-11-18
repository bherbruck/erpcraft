# erpcraft-webhooks

Sends committed changes to HTTP endpoints outside the server. It is an
outbox subscriber like any other and uses only the public module API.

## What a receiver gets

One POST per event, with a JSON body:

```json
{
  "id": "0198…",
  "topic": "journal_entry.post",
  "created_at": "2026-03-15T10:00:00Z",
  "payload": { "command": "journal_entry.post", "user_id": "…", "input": {}, "output": {} }
}
```

The payload is what the outbox holds. For a command that handles secrets
it has `command` and `user_id` only.

| Header | Value |
|---|---|
| `X-Erpcraft-Event` | the topic, a command name |
| `X-Erpcraft-Delivery` | the event id; the same on every retry |
| `X-Erpcraft-Signature` | `sha256=` + hex HMAC-SHA256 of the raw body under the webhook's secret |

A receiver should:

1. compute the HMAC over the bytes it received, before parsing them, and
   compare it with the header in constant time;
2. answer 2xx within 10 seconds, and do slow work afterwards;
3. tolerate a repeat: delivery is at least once, so remember the
   `X-Erpcraft-Delivery` ids it has handled;
4. not rely on order.

Any other answer, a timeout or a connection error fails the delivery. It
is retried after 10 seconds, then with a doubling delay up to an hour.
Redirects are not followed.

## Managing webhooks

| Command | |
|---|---|
| `webhook.create` | name, url, topics, company_id, secret, active. Returns the secret, once. |
| `webhook.update` | changes anything but the secret |
| `webhook.set_secret` | replaces the secret; a generated one is returned, once |
| `webhook.delete` | |
| `webhook.deliveries` | what was sent to one webhook, and why it failed |

Webhooks are shared by all companies, so these need a grant for all
companies (`webhook:create`, `webhook:update`, `webhook:delete`,
`webhook:read`). The `webhook` entity can be listed and read through the
generic API, never written.

Topics are command names (`journal_entry.post`) or a prefix ending in `*`
(`purchase_order.*`). No topics means every event.

With `company_id`, a webhook gets only the events of that company. The
company of an event is read from its payload: `output.company_id`, else
`input.company_id`. Events that name no company (records shared by all
companies, commands that handle secrets) go only to webhooks without a
company.

## The secret

- Generated when none is given: `whsec_` and 64 hex digits.
- Returned by `webhook.create`, and by `webhook.set_secret` when it
  generates one. No command or entity read returns it afterwards.
- The commands that take it are marked sensitive: their input and output
  are not written to the audit log, the outbox or the idempotency table.
  The same holds for the URL, which is often a credential itself.
- Stored as given in the table `webhook_secret`, because signing needs the
  secret itself, not a hash. It is protected by access to the database
  and nothing else: whoever can read that table can sign as the server.
  The table is not an entity, so no generic read can reach it.

## Where a webhook may point

Over https, to a public address, and without a user name or password in
the URL. Public is decided by an allow-list: the address must lie in the
global unicast space of its family (IPv4 1.0.0.0 to 223.255.255.255, IPv6
`2000::/3`) and outside the blocks there that the special-purpose
registries mark as not globally reachable: private, carrier-shared,
loopback, link-local (with the clouds' metadata services), documentation,
benchmarking and the like. An IPv6 address that carries an IPv4 address
(IPv4-mapped, IPv4-compatible, NAT64 `64:ff9b::/96`, 6to4 `2002::/16`) is
judged by that IPv4 address; Teredo (`2001::/32`) is refused whole.

Every such refusal reads the same, whether the address is private, the
name leads to a private address, or the name does not exist, and it names
no address: otherwise whoever may save a webhook could map the names of
the internal network. The reason is written to the server's log.

The check is made when a webhook is saved and again for every request,
inside the name lookup of the HTTP client, so a name cannot be checked at
one address and reached at another. Proxies from the environment are not
used, for the same reason.

For development, `ERPCRAFT_WEBHOOKS_ALLOW_PRIVATE=1` in the server's
environment lifts the address check, and plain http is then accepted for
`localhost`, `127.0.0.1` and `[::1]`. It is read once, at startup.
