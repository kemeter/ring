CREATE TABLE IF NOT EXISTS volumes (
    id VARCHAR(255) NOT NULL,
    name varchar(255) NOT NULL,
    namespace varchar(255) NOT NULL,
    size INTEGER DEFAULT NULL,
    backend_type varchar(255) NOT NULL,
    host_path varchar(255) NOT NULL,
    labels JSON NOT NULL,
    created_at datetime NOT NULL,
    updated_at datetime DEFAULT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS volumes_namespace_name ON volumes (namespace, name);
