-- Durable outbound event queue (outbox pattern).
--
-- Any source in Ring publishes a typed event by inserting a row here; a
-- dedicated async worker (scheduler/event_worker) picks up due rows and
-- delivers them to the webhooks subscribed to that `kind`, with exponential
-- backoff and a dead-letter terminal state.
--
-- Persisting the event before delivery is the whole point: an event survives
-- a subscriber being down or a ring-server restart. Best-effort fire-and-forget
-- would lose it. Clients should still treat webhooks as a latency reducer, not
-- the sole source of truth (the REST API remains authoritative).
CREATE TABLE IF NOT EXISTS events (
    id VARCHAR(255) PRIMARY KEY,
    created_at DATETIME NOT NULL,
    updated_at DATETIME,
    -- Stable event discriminant, e.g. "deployment.status_changed".
    kind VARCHAR(100) NOT NULL,
    -- Event-specific JSON body, delivered verbatim to subscribers.
    payload JSON NOT NULL,
    -- "pending" (to deliver) / "delivered" (done) / "dead" (gave up after
    -- MAX_ATTEMPTS — kept for inspection, never retried).
    status VARCHAR(20) NOT NULL,
    -- Delivery attempts so far; drives the backoff schedule.
    attempts INTEGER NOT NULL DEFAULT 0,
    -- Earliest time the worker may (re)try this event. Set to now on enqueue,
    -- pushed forward by the backoff on each failure.
    next_attempt_at DATETIME NOT NULL,
    -- Last delivery error, for debugging a dead-lettered row.
    last_error TEXT
);

-- The worker's hot query: pending rows whose next_attempt_at is due.
CREATE INDEX IF NOT EXISTS idx_events_status_next_attempt ON events(status, next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_events_created_at ON events(created_at);
