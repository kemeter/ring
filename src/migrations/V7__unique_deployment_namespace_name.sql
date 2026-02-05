CREATE UNIQUE INDEX deployment_namespace_name_active
ON deployment (namespace, name)
WHERE status NOT IN ('deleted');
