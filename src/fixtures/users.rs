use sqlx::SqlitePool;

/// Valid argon2id hash for the password "changeme" (verified via
/// `argon2::verify_encoded`). The previous fixture hash was bogus — it never
/// matched "changeme", so login tests only ever passed via the migration-seeded
/// admin account. Keep this in sync if the password ever changes.
const CHANGEME_HASH: &str =
    "$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$HxyGA81ORfjb63QVOi3+t/eBaFPmdSbf4OZc4pBG8DM";

pub async fn load(pool: &SqlitePool) {
    // The admin account (id 1c5a5fe9-…, username "admin") is seeded by
    // migration 0001. We don't re-insert it (PK clash) and we don't touch the
    // migration. We only promote it to role='admin' here so cross-account
    // tests (update/delete other users) exercise the admin path.
    sqlx::query("UPDATE user SET role = 'admin' WHERE id = ?")
        .bind("1c5a5fe9-84e0-4a18-821e-8058232c2c23")
        .execute(pool)
        .await
        .unwrap();

    // A deletable target account for the delete() test. Plain 'user', distinct
    // username so it never collides with the admin login lookup.
    sqlx::query(
        "INSERT INTO user (id, created_at, status, role, username, password, token) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("active")
        .bind("user")
        .bind("deletable.user")
        .bind(CHANGEME_HASH)
        .bind("deletabletoken")
        .execute(pool)
        .await
        .unwrap();

    // A second plain 'user' used to assert a non-admin cannot touch other
    // accounts (IDOR regression tests).
    sqlx::query(
        "INSERT INTO user (id, created_at, status, role, username, password, token) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("6c6d481b-debf-5gb5-937f-2ffa5e9f8e58")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("active")
        .bind("user")
        .bind("john.doe")
        .bind(CHANGEME_HASH)
        .bind("john_token")
        .execute(pool)
        .await
        .unwrap();
}
