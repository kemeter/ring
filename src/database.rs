use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteConnectOptions};
use std::env;
use std::str::FromStr;

pub(crate) async fn get_database_pool() -> SqlitePool {
    let database_path = env::var("RING_DATABASE_PATH")
        .unwrap_or_else(|_| "ring.db".to_string());

    let connect_options = SqliteConnectOptions::from_str(&format!("sqlite:{}", database_path))
        .expect("Invalid database path")
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);

    let max_connections = env::var("RING_DB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(5);

    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect_with(connect_options)
        .await
        .expect("Could not create database pool")
}

/// Automatically migrate from refinery to sqlx if needed.
/// Detects if refinery_schema_history exists and _sqlx_migrations does not,
/// then creates _sqlx_migrations with all existing migrations marked as applied.
pub(crate) async fn migrate_from_refinery_if_needed(pool: &SqlitePool) {
    let has_refinery: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='refinery_schema_history'"
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_refinery {
        return;
    }

    let has_sqlx: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'"
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if has_sqlx {
        return;
    }

    info!("Detected refinery migration history, transitioning to sqlx...");

    // Get the number of migrations applied by refinery
    let refinery_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM refinery_schema_history"
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // Use sqlx's embedded migrations to get the correct checksums
    let migrator = sqlx::migrate!("./migrations");

    sqlx::query(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            success BOOLEAN NOT NULL,
            checksum BLOB NOT NULL,
            execution_time BIGINT NOT NULL
        )"
    )
    .execute(pool)
    .await
    .expect("Failed to create _sqlx_migrations table");

    // Mark the first N migrations as applied (matching refinery count)
    for migration in migrator.iter().take(refinery_count as usize) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time)
             VALUES (?, ?, CURRENT_TIMESTAMP, 1, ?, 0)"
        )
        .bind(migration.version)
        .bind(&*migration.description)
        .bind(&*migration.checksum)
        .execute(pool)
        .await
        .expect("Failed to insert migration record");
    }

    info!("Refinery to sqlx migration transition complete ({} migrations marked as applied)", refinery_count);
}
