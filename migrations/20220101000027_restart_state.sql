-- Restart state, persisted so a `ring server` restart no longer resets every
-- failing deployment's backoff at once.
--
--   `next_attempt_at`   when the scheduler may start an instance again, NULL
--                       when it may do so now.
--   `running_since`     start of the current uninterrupted run, which resets
--                       `restart_count` once it lasts `stable_after`.
--   `last_termination`  the last instance exit (code, time, tail of its logs),
--                       JSON, kept after the instance itself is removed.
--
-- Timestamps are RFC 3339 UTC.
ALTER TABLE deployment ADD COLUMN next_attempt_at TEXT DEFAULT NULL;
ALTER TABLE deployment ADD COLUMN running_since TEXT DEFAULT NULL;
ALTER TABLE deployment ADD COLUMN last_termination JSON DEFAULT NULL;
