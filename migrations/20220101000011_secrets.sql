CREATE TABLE IF NOT EXISTS secret (
    id VARCHAR(255) NOT NULL PRIMARY KEY,
    created_at DATETIME NOT NULL,
    updated_at DATETIME DEFAULT NULL,
    namespace VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    value BLOB NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS secret_namespace_name ON secret (namespace, name);
