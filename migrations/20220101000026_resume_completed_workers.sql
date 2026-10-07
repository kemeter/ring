-- Resume workers left in Completed by earlier versions.
--
-- A worker that exited 0 used to be marked Completed, which the scheduler never
-- reconciles again, so the service stayed down for good (typically after a host
-- reboot, when services shut down cleanly). Workers are now recreated on any
-- exit and only jobs can be Completed: put the stranded workers back under
-- reconciliation with a fresh restart budget.
UPDATE deployment
SET status = 'running', restart_count = 0
WHERE kind = 'worker' AND status = 'completed';
