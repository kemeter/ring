-- Why some of a deployment's instances are not running while others serve.
--
-- `reason` / `message` only say why a deployment is backing off or failed. A
-- deployment that keeps at least one instance up stays in its status when
-- another crashes or cannot start (memory, a missing config...), so the
-- operator saw `running` and `1/3` with nothing saying why. These columns keep
-- the last such failure until the deployment reaches its replica count.
ALTER TABLE deployment ADD COLUMN waiting_reason TEXT DEFAULT NULL;
ALTER TABLE deployment ADD COLUMN waiting_message TEXT DEFAULT NULL;
