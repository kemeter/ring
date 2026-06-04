-- Webhook subscribers.
--
-- Each row is an HTTP endpoint that wants to receive events. `events` is the
-- subscription filter: a JSON array of event kinds (e.g.
-- ["deployment.status_changed"]); an empty array means "all kinds". When an
-- event is due, the worker delivers it to every active webhook whose filter
-- matches the event's kind.
--
-- Declared dynamically via POST /webhooks (CLI: `ring webhook create`), so
-- subscribers can be added/removed at runtime without restarting the server.
CREATE TABLE IF NOT EXISTS webhook (
    id VARCHAR(255) PRIMARY KEY,
    created_at DATETIME NOT NULL,
    -- Target URL that receives the signed POST.
    url TEXT NOT NULL,
    -- Optional HMAC-SHA256 shared secret. When set, deliveries carry an
    -- X-Ring-Signature: sha256=<hex> header the receiver can verify. Stored as
    -- given (it's the subscriber's own secret, not a Ring credential).
    secret TEXT,
    -- JSON array of subscribed event kinds; [] = all kinds. Same storage
    -- pattern as deployment.labels / token.scopes.
    events JSON NOT NULL,
    -- Soft delete: non-NULL means revoked (no longer receives deliveries).
    revoked_at DATETIME
);
