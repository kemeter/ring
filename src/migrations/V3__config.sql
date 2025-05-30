CREATE TABLE config (
    id VARCHAR(255) NOT NULL,
    created_at datetime NOT NULL,
    updated_at datetime DEFAULT NULL,
    namespace varchar(255) NOT NULL,
    name varchar(255) NOT NULL,
    data JSON NOT NULL,
    labels JSON NOT NULL
);

CREATE UNIQUE INDEX config_namespace_name ON config (namespace, name);
