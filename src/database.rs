use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::env;
use std::str::FromStr;
use std::time::Duration;

pub(crate) async fn get_database_pool() -> SqlitePool {
    let database_path = env::var("RING_DATABASE_PATH").unwrap_or_else(|_| "ring.db".to_string());

    let connect_options = SqliteConnectOptions::from_str(&format!("sqlite:{}", database_path))
        .expect("Invalid database path")
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        // Without a busy timeout, a write that hits a momentarily locked
        // database (another connection mid-write) fails instantly with
        // "database is locked". WAL allows concurrent readers but still
        // serializes writers, and the scheduler + API can write at the same
        // time. 5s lets SQLite retry internally instead of bubbling the error.
        .busy_timeout(Duration::from_secs(5));

    let max_connections = env::var("RING_DB_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(5);

    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(connect_options)
        .await
        .expect("Could not create database pool")
}

/// Automatically migrate from refinery to sqlx if needed.
/// Detects if refinery_schema_history exists and _sqlx_migrations holds no
/// migration yet, then records the migrations refinery applied as applied.
///
/// The table and its rows are written in one transaction: a server stopped
/// half-way used to leave an empty `_sqlx_migrations`, which this function
/// then skipped, and sqlx ran the first migration again against the existing
/// schema, failing every start. An empty table left by such a run is filled
/// in now.
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
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if has_sqlx {
        let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await
            .unwrap_or(0);
        if recorded > 0 {
            return;
        }
    }

    info!("Detected refinery migration history, transitioning to sqlx...");

    // Get the number of migrations applied by refinery
    let refinery_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM refinery_schema_history")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    // Use sqlx's embedded migrations to get the correct checksums
    let migrator = sqlx::migrate!("./migrations");

    let mut tx = pool
        .begin()
        .await
        .expect("Failed to start the migration transition");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            success BOOLEAN NOT NULL,
            checksum BLOB NOT NULL,
            execution_time BIGINT NOT NULL
        )",
    )
    .execute(&mut *tx)
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
        .execute(&mut *tx)
        .await
        .expect("Failed to insert migration record");
    }
    tx.commit()
        .await
        .expect("Failed to commit the migration transition");

    info!(
        "Refinery to sqlx migration transition complete ({} migrations marked as applied)",
        refinery_count
    );
}

#[cfg(test)]
mod tests {
    use super::migrate_from_refinery_if_needed;
    use sqlx::sqlite::SqlitePoolOptions;

    /// A refinery-era database whose transition was cut short after creating
    /// an empty `_sqlx_migrations` must still start.
    #[tokio::test]
    async fn an_interrupted_transition_from_refinery_is_completed() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let migrator = sqlx::migrate!("./migrations");
        migrator.run(&pool).await.unwrap();
        let applied = migrator.iter().count();
        // The schema in place, a refinery history, and the empty table a
        // stopped transition left.
        sqlx::query("DELETE FROM _sqlx_migrations")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE refinery_schema_history \
             (version INT4 PRIMARY KEY, name VARCHAR(255), applied_on VARCHAR(255), checksum VARCHAR(255))",
        )
        .execute(&pool)
        .await
        .unwrap();
        for version in 1..=applied {
            sqlx::query("INSERT INTO refinery_schema_history VALUES (?, 'm', 'now', '0')")
                .bind(version as i64)
                .execute(&pool)
                .await
                .unwrap();
        }

        migrate_from_refinery_if_needed(&pool).await;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("the existing schema must not be migrated again");
    }
}
