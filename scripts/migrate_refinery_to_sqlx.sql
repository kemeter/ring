-- Migration script: Refinery -> sqlx
-- Run this ONCE on existing production databases before deploying the new binary.
--
-- Usage: sqlite3 ring.db < scripts/migrate_refinery_to_sqlx.sql

-- Create the sqlx migrations table
CREATE TABLE IF NOT EXISTS _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
);

-- Register all existing migrations as already applied
-- The checksums are placeholders (empty) - sqlx will not re-validate them
INSERT OR IGNORE INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time)
VALUES
    (20220101000001, 'initial',                          CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000002, 'restart',                          CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000003, 'config',                           CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000004, 'command',                          CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000005, 'deployment events',                CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000006, 'health checks',                    CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000007, 'unique deployment namespace name', CURRENT_TIMESTAMP, 1, X'', 0),
    (20220101000008, 'seed admin',                       CURRENT_TIMESTAMP, 1, X'', 0);

-- Verify
SELECT 'Migration transition complete. ' || COUNT(*) || ' migrations registered.' FROM _sqlx_migrations;
