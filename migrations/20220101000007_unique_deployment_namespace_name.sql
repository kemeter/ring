CREATE UNIQUE INDEX IF NOT EXISTS deployment_namespace_name_active
ON deployment (namespace, name)
WHERE status NOT IN ('deleted');
