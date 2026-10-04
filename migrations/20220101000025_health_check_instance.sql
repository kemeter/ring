-- Record which instance a health check result is about.
--
-- Results were stored per deployment only, so a failure could not be told
-- apart by instance: a replica that was already gone, or one that recovered,
-- looked the same as the one still failing. NULL for the results stored before
-- this column existed.
ALTER TABLE health_check ADD COLUMN instance_id VARCHAR(255) DEFAULT NULL;
