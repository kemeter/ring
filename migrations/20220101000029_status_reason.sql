-- Split `deployment.status` into a status and a reason.
--
-- `status` mixed where a deployment is in its lifecycle with why it is there.
-- It now keeps the former only: pending, progressing, running, backing_off,
-- completed, failed or deleted. `reason` says why a deployment is backing off
-- or failed, and `message` gives the detail.
--
-- Every error status becomes `backing_off` with its reason. A worker that was
-- `failed` (a readiness deadline, a missing firmware) was abandoned for good; it
-- is now retried like any other, so it lands in `backing_off` too. Every
-- deployment landing in `backing_off` starts over: a fresh counter and its next
-- attempt right away, so the workers abandoned before this change are retried
-- on the first scheduler tick.
--
-- A `failed` job stays failed: `status` cannot tell a job that never started
-- from one that ran, and running a job twice is worse than asking for a
-- `ring apply`.
ALTER TABLE deployment ADD COLUMN reason TEXT DEFAULT NULL;
ALTER TABLE deployment ADD COLUMN message TEXT DEFAULT NULL;

-- The reason is read from the former status, so it is set first.
UPDATE deployment SET reason = CASE status
        WHEN 'crash_loop_back_off' THEN 'crash_loop'
        WHEN 'image_pull_back_off' THEN 'image_pull'
        WHEN 'create_container_error' THEN 'spec_rejected'
        WHEN 'config_error' THEN 'config_missing'
        WHEN 'insufficient_resources' THEN 'insufficient_resources'
        WHEN 'network_error' THEN 'network'
        WHEN 'file_system_error' THEN 'file_system'
        WHEN 'error' THEN 'error'
        WHEN 'failed' THEN CASE kind WHEN 'job' THEN 'run_failed' ELSE 'readiness_timeout' END
        ELSE NULL
    END;

UPDATE deployment SET status = CASE
        WHEN status = 'creating' THEN 'progressing'
        WHEN status IN ('pending', 'running', 'completed', 'deleted') THEN status
        WHEN status = 'failed' AND kind = 'job' THEN 'failed'
        ELSE 'backing_off'
    END;

UPDATE deployment
SET restart_count = 0, next_attempt_at = NULL, running_since = NULL
WHERE status = 'backing_off';
