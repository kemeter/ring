-- Lightweight audit log of write actions, scoped by namespace.
--
-- Purpose: "who did what" — e.g. <user-id> created deployment <name> in
-- namespace <ns>. This is intentionally NOT a clone of deployment_event
-- (which is per-deployment runtime telemetry): it survives the deletion of
-- an individual deployment so a post-mortem stays possible after cleanup.
--
-- Retention is tied to the namespace: rows are kept regardless of the
-- target's lifecycle, but the whole namespace's audit trail is removed when
-- the namespace itself is deleted (handled in application code, no FK so the
-- log can outlive a deleted target row).
CREATE TABLE IF NOT EXISTS audit_log (
    id VARCHAR(255) PRIMARY KEY,
    timestamp DATETIME NOT NULL,
    -- Author: the authenticated user's id (User.id from AuthContext). May be
    -- NULL only for actions with no resolvable user (defensive; not expected).
    user_id VARCHAR(255),
    -- e.g. "create", "update", "delete".
    action VARCHAR(50) NOT NULL,
    -- e.g. "deployment", "secret", "config", "namespace".
    target_type VARCHAR(50) NOT NULL,
    -- Human-facing name of the affected resource (deployment name, etc.).
    target_name VARCHAR(255) NOT NULL,
    -- Namespace the action belongs to. NULL for namespace-level actions that
    -- have no parent namespace context.
    namespace VARCHAR(255)
);

CREATE INDEX IF NOT EXISTS idx_audit_log_namespace ON audit_log(namespace);
CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON audit_log(timestamp);
