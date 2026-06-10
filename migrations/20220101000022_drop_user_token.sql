-- The plaintext per-user session token (`user.token`) is gone. Login sessions
-- now live in the `token` table (the same table as PATs): hashed at rest,
-- regenerated on every login, revocable via /logout. See migration
-- 20220101000018_scoped_api_tokens.sql and src/api/action/login.rs.
--
-- Existing `user.token` values were never real sessions worth keeping (UUID
-- seeds / a single shared secret), so there is nothing to migrate — active
-- clients simply log in again.
ALTER TABLE user DROP COLUMN token;

-- Distinguish a login session from a PAT with a first-class `kind` column
-- instead of overloading the free-text `name` (where a user-created PAT named
-- "session" would have silently collided with the session-hiding filter). A
-- session is `kind = 'session'`, every other token is `kind = 'pat'`.
--
--   * minted by /login          -> 'session'  (src/api/action/login.rs)
--   * minted by `ring token`    -> 'pat'      (src/api/action/token/create.rs)
--   * listed by `ring token list`-> WHERE kind = 'pat' (sessions stay hidden)
--   * addressable by id         -> sessions are managed only via /logout
--
-- DEFAULT 'pat' reclassifies every pre-existing PAT correctly; the backfill
-- below promotes any session rows already minted on this branch (which used
-- name = 'session' as the old marker) to the new kind.
ALTER TABLE token ADD COLUMN kind TEXT NOT NULL DEFAULT 'pat';
UPDATE token SET kind = 'session' WHERE name = 'session';
