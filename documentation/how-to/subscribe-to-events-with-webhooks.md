# Subscribe to events with webhooks

Instead of polling `GET /deployments/{id}` to find out when a deployment changes state, register a **webhook**: Ring POSTs to your endpoint every time a subscribed event fires. Deliveries go through a durable queue with retry, so a brief outage on your side doesn't drop events.

## Register a webhook

```bash
ring webhook create https://hooks.example.com/ring --event deployment.status_changed
```

Ring prints the webhook id on stdout and, on stderr, the generated signing secret, which is **shown once**:

```
Webhook registered for https://hooks.example.com/ring
  events: deployment.status_changed
  secret: whsec_3f9a… (copy it now — not shown again)
9f1c2d3e-…
```

Omit `--event` to receive every event kind. Pass `--secret <value>` to use your own secret instead of a generated one.

Managing webhooks needs an API token with the `webhooks:write` scope (see [API tokens](./authenticate-scripts-with-tokens.md)).

## Receive and verify a delivery

Each delivery is a JSON POST with these headers:

- `X-Ring-Event: deployment.status_changed`: the event kind
- `X-Ring-Signature: sha256=<hmac>`: HMAC-SHA256 of the raw body, keyed by your secret (only when the webhook has a secret)

Verify the signature before trusting the body. In Python:

```python
import hmac, hashlib

def verify(secret: str, body: bytes, header: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(expected, header)
```

The body for `deployment.status_changed`:

```json
{
  "schema_version": 1,
  "deployment_id": "f3a8b2c4-...",
  "namespace": "production",
  "name": "web",
  "kind": "worker",
  "old_status": "creating",
  "new_status": "running",
  "restart_count": 0
}
```

Respond with any `2xx` to acknowledge. A non-2xx (or a timeout) makes Ring retry with exponential backoff; after repeated failures the event is dead-lettered and stops being retried.

### Other event kinds

`deployment.status_changed` is the headline event, but Ring emits more. Subscribe to all of them by omitting `--event`, or pick specific ones (`--event` is repeatable):

- `deployment.health_check_failed`: a probe failed and its `on_failure` action (restart / stop / alert) fired
- `deployment.rolling_update`: a rollout drained an instance, completed, or failed
- `deployment.scaled`: the reconciler added or removed an instance
- `deployment.error`: the runtime couldn't bring a deployment up, with a `reason` and a `category` (`user` / `host` / `transient`)

See [API reference → Webhooks](/documentation/reference/api#webhooks) for each payload, and [Deployment status lifecycle](/documentation/concepts/deployment-status-lifecycle) for how these relate to a deployment's status.

## Idempotency

Delivery is **at-least-once**: on a retry, an event your endpoint already processed may arrive again. Key your handling on `deployment_id` + `new_status` (or carry your own dedup) so reprocessing is a no-op.

## List and remove

```bash
ring webhook list
ring webhook delete <ID>
```

`list` shows each webhook's URL, subscribed events and status (`active`/`revoked`); secrets are never shown. `delete` stops further deliveries.
