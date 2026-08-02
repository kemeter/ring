-- Turns the flat user/admin role of migration 0016 into real RBAC:
-- 'admin', 'operator' and 'viewer', constrained by a CHECK.
--
-- The column added by 0016 carries no constraint, so any string can currently
-- land in it. SQLite cannot add a CHECK to an existing column, hence the table
-- rebuild below.
--
-- Role mapping:
--   'admin' -> 'admin'    (kept as-is; who is admin was decided out of band)
--   anything else         -> 'viewer'  (fail-safe: least privilege)
--
-- The old 'user' role was self-scoped for account management, but carried a
-- full-access session in practice (every login minted scopes=["admin"]).
-- Mapping it to 'viewer' is therefore a deliberate privilege REDUCTION, and a
-- breaking change for those accounts.

PRAGMA foreign_keys = off;

-- Mirrors the post-0022 shape of `user` exactly (same columns, same nullability,
-- no PK/unique index -- none existed), plus the CHECK on `role`. Widening the
-- schema here would smuggle an unrelated change into an RBAC migration.
CREATE TABLE user_new (
    id VARCHAR(255) NOT NULL,
    created_at datetime NOT NULL,
    updated_at datetime DEFAULT NULL,
    status VARCHAR(255) NOT NULL,
    role VARCHAR(50) NOT NULL DEFAULT 'viewer'
        CHECK (role IN ('admin', 'operator', 'viewer')),
    username VARCHAR(255) NOT NULL,
    password VARCHAR(255) NOT NULL,
    login_at datetime DEFAULT NULL
);

INSERT INTO user_new (id, created_at, updated_at, status, role, username, password, login_at)
SELECT
    id,
    created_at,
    updated_at,
    status,
    CASE WHEN role = 'admin' THEN 'admin' ELSE 'viewer' END,
    username,
    password,
    login_at
FROM user;

DROP TABLE user;
ALTER TABLE user_new RENAME TO user;

PRAGMA foreign_keys = on;

-- Keep the seed account administrable.
--
-- Migration 0016 defaulted EVERY row to 'user' and deliberately promoted no
-- one, so the seed admin from 0001 carries role='user' on every install that
-- never set it by hand. The fail-safe mapping above would therefore demote it
-- to 'viewer' and leave the server with no admin at all -- locking the operator
-- out of their own instance. Match on the seed's fixed id (not on the username,
-- which is editable) and only when no admin survived the rebuild.
UPDATE user
SET role = 'admin'
WHERE id = '1c5a5fe9-84e0-4a18-821e-8058232c2c23'
  AND NOT EXISTS (SELECT 1 FROM user WHERE role = 'admin');

-- Revoke every live credential.
--
-- Scopes are frozen into the `token` row when it is minted, not recomputed per
-- request. Without this, every session and PAT issued before the migration
-- keeps the scopes it was born with -- including the hard-coded ["admin"] that
-- every login used to get -- and demoting a user would grant nothing back.
-- Revoking here is what makes the new roles take effect at all.
--
-- Consequence: everyone is logged out and every PAT stops working. Admins
-- re-login and re-issue tokens; this is called out as a breaking change.
UPDATE token SET revoked_at = datetime() WHERE revoked_at IS NULL;
