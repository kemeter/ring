use sqlx::SqlitePool;

pub async fn load(pool: &SqlitePool) {
    // Admin user for tests - using the same ID as the original fixture
    sqlx::query(
        "INSERT INTO user (id, created_at, status, username, password, token) VALUES (?, ?, ?, ?, ?, ?)"
    )
        .bind("5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("active")
        .bind("admin")
        .bind("$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$NtAhPV3e8INMg6E1LnAE5wIHd/YszYoEyZeF0+1zT8E") // changeme
        .bind("johndoetoken") // Keep the same token as before to not break existing tests
        .execute(pool)
        .await
        .unwrap();

    // Second user for tests that need multiple users
    sqlx::query(
        "INSERT INTO user (id, created_at, status, username, password, token) VALUES (?, ?, ?, ?, ?, ?)"
    )
        .bind("6c6d481b-debf-5gb5-937f-2ffa5e9f8e58")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("active")
        .bind("john.doe")
        .bind("$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$NtAhPV3e8INMg6E1LnAE5wIHd/YszYoEyZeF0+1zT8E") // changeme
        .bind("john_token")
        .execute(pool)
        .await
        .unwrap();
}
