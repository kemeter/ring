-- Per-deployment restart settings and job run failures.
--
--   `restart`       the manifest's `restart` block, JSON, NULL when it has
--                   none: every key it leaves out falls back to the server's
--                   `[server.restart]`.
--   `run_failures`  runs of a job that exited non-zero, bounded by its
--                   `backoff_limit`. Start failures do not count: nothing ran.
ALTER TABLE deployment ADD COLUMN restart JSON DEFAULT NULL;
ALTER TABLE deployment ADD COLUMN run_failures INTEGER NOT NULL DEFAULT 0;
