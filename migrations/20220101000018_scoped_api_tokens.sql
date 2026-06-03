-- Scoped API tokens (Personal Access Tokens).
--
-- Purpose: let scripts, CI and external agents talk to the API without
-- borrowing a human's login session (user.token). Each token carries its own
-- scopes (verb:resource), an optional namespace boundary, an optional
-- expiry, and can be revoked or rotated individually.
--
-- This is intentionally a separate table from user.token (the dashboard/CLI
-- session credential): a PAT is hashed at rest (never stored in clear),
-- revocable, and there can be many per user. The auth middleware tries a PAT
-- lookup first, then falls back to the session token.
CREATE TABLE IF NOT EXISTS token (
    id VARCHAR(255) PRIMARY KEY,
    -- Owner: the user.id this token acts on behalf of.
    user_id VARCHAR(255) NOT NULL,
    -- Human-facing label, e.g. "ci-deploy".
    name VARCHAR(255) NOT NULL,
    -- SHA-256 of the clear token. The clear value (ring_pat_<random>) is shown
    -- once at creation and never stored. Tokens are high-entropy secrets, not
    -- passwords, so a fast hash with a constant-time DB lookup is appropriate
    -- (unlike user.password, which uses Argon2).
    token_hash VARCHAR(255) NOT NULL,
    -- First chars of the clear token (e.g. "ring_pat_a1b2c3"), safe to display
    -- in listings so a token can be identified without revealing its secret.
    token_prefix VARCHAR(255) NOT NULL,
    -- JSON array of scopes, e.g. ["deployments:read","secrets:write"].
    -- Same storage pattern as deployment.labels / deployment.volumes.
    scopes JSON NOT NULL,
    -- JSON array of namespaces this token is scoped to. Empty array = all.
    namespaces JSON NOT NULL,
    created_at DATETIME NOT NULL,
    -- NULL = never expires.
    expire_at DATETIME,
    -- Best-effort last-use marker, throttled to at most one write per minute.
    last_used_at DATETIME,
    -- Soft delete: non-NULL means revoked. Kept (not deleted) so the audit
    -- trail referencing this token stays coherent.
    revoked_at DATETIME
);

-- Auth lookup is by hash on every authenticated request: index it.
CREATE INDEX IF NOT EXISTS idx_token_token_hash ON token(token_hash);
-- Listing a user's tokens.
CREATE INDEX IF NOT EXISTS idx_token_user_id ON token(user_id);
