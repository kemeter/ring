-- Which of a deployment's health checks a result belongs to: its position in
-- the manifest's `health_checks`.
--
-- Results were told apart by `check_type` only, so two checks of the same type
-- (two http probes) shared one stream of results, and the readiness gate,
-- expecting one result per check, never saw enough to open. NULL for results
-- stored before this column existed.
ALTER TABLE health_check ADD COLUMN check_index INTEGER DEFAULT NULL;
