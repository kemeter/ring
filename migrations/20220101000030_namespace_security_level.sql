-- The security level of a namespace: what its deployments may ask of the
-- host. NULL follows the server's `[server.admission] default`.
ALTER TABLE namespace ADD COLUMN security_level TEXT DEFAULT NULL
    CHECK (security_level IN ('privileged', 'baseline', 'restricted'));

-- Namespaces already running a deployment that reaches the host keep allowing
-- it, so the upgrade does not stop them: they are pinned to `privileged`, for
-- the operator to review. Deployments made before namespaces were stored may
-- live in a namespace without a row, which is created for the occasion. A
-- column that does not hold valid JSON is skipped rather than failing the
-- migration.
CREATE TEMP TABLE reaching_the_host AS
SELECT DISTINCT namespace AS name
FROM deployment
WHERE status <> 'deleted'
  AND (
    network_mode = 'host'
    OR CASE WHEN json_valid(config) THEN json_extract(config, '$.user.privileged') END = 1
    OR EXISTS (
        SELECT 1
        FROM json_each(CASE WHEN json_valid(deployment.volumes) THEN deployment.volumes ELSE '[]' END)
        WHERE json_extract(json_each.value, '$.type') = 'bind'
    )
  );

INSERT OR IGNORE INTO namespace (id, created_at, updated_at, name)
SELECT
    lower(
        substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-4' || substr(h, 14, 3) || '-a'
        || substr(h, 18, 3) || '-' || substr(h, 21, 12)
    ),
    datetime('now'),
    NULL,
    name
FROM (SELECT hex(randomblob(16)) AS h, name FROM reaching_the_host);

UPDATE namespace SET security_level = 'privileged'
WHERE name IN (SELECT name FROM reaching_the_host);

DROP TABLE reaching_the_host;
