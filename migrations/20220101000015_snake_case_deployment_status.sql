-- Rewrite deployment.status to snake_case for the error variants so the
-- column has a single, consistent convention. Before this migration the
-- lifecycle states were lowercase (`running`, `pending`, …) while the
-- error states were PascalCase (`CrashLoopBackOff`, `ImagePullBackOff`, …),
-- which silently dropped rows from string-matching filters elsewhere in
-- the codebase.
--
-- The new canonical names (snake_case) match what `DeploymentStatus`'s
-- `Display` / `FromStr` / serde all produce after this PR.
--
-- This is a breaking change for any external consumer that parsed the
-- JSON API output for status — see the CHANGELOG entry for the mapping.

UPDATE deployment SET status = 'crash_loop_back_off'    WHERE status = 'CrashLoopBackOff';
UPDATE deployment SET status = 'image_pull_back_off'    WHERE status = 'ImagePullBackOff';
UPDATE deployment SET status = 'create_container_error' WHERE status = 'CreateContainerError';
UPDATE deployment SET status = 'network_error'          WHERE status = 'NetworkError';
UPDATE deployment SET status = 'config_error'           WHERE status = 'ConfigError';
UPDATE deployment SET status = 'file_system_error'      WHERE status = 'FileSystemError';
UPDATE deployment SET status = 'error'                  WHERE status = 'Error';
