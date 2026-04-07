ALTER TABLE deployment ADD COLUMN parent_id TEXT DEFAULT NULL;

DROP INDEX IF EXISTS deployment_namespace_name_active;

CREATE UNIQUE INDEX deployment_namespace_name_active
ON deployment (namespace, name)
WHERE status NOT IN ('deleted') AND parent_id IS NULL;
