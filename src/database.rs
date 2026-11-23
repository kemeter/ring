use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::env;
use std::str::FromStr;
use std::time::Duration;

/// The database file the server uses: `RING_DATABASE_PATH`, or `ring.db`.
pub(crate) fn database_path() -> String {
    env::var("RING_DATABASE_PATH").unwrap_or_else(|_| "ring.db".to_string())
}

/// Held for the life of a server: only one may use a database.
///
/// Two servers on one database both reconciled every deployment, starting
/// replicas twice over (15 containers for 12 deployments of one replica) and
/// delivering each webhook twice, since the event queue is read without a
/// claim. The lock is an advisory one on `<database>.lock`, released by the
/// kernel when the process ends, however it ends.
pub(crate) struct ServerLock {
    _file: std::fs::File,
}

/// Take the server lock for `database`, or say which process holds it.
pub(crate) fn lock_database(database: &str) -> Result<ServerLock, String> {
    use std::io::Write;

    let path = format!("{database}.lock");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("cannot open {path}: {e}"))?;
    match file.try_lock() {
        Ok(()) => {
            let _ = file.set_len(0);
            let _ = write!(file, "{}", std::process::id());
            Ok(ServerLock { _file: file })
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let holder = std::fs::read_to_string(&path).unwrap_or_default();
            Err(format!(
                "another Ring server{} is already using the database {database}; \
                 stop it, or give this one its own RING_DATABASE_PATH",
                if holder.trim().is_empty() {
                    String::new()
                } else {
                    format!(" (pid {})", holder.trim())
                }
            ))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot lock {path}: {e}")),
    }
}

pub(crate) async fn get_database_pool() -> SqlitePool {
    let database_path = database_path();

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

#[cfg(test)]
mod lock_tests {
    #[test]
    fn a_second_server_cannot_take_the_lock() {
        let db = std::env::temp_dir().join(format!("ring-lock-{}.db", uuid::Uuid::new_v4()));
        let db = db.to_string_lossy().to_string();

        let first = super::lock_database(&db).expect("the first server takes the lock");
        let second = super::lock_database(&db);
        match second {
            Err(msg) => {
                assert!(msg.contains("already using the database"), "{msg}");
                assert!(
                    msg.contains(&std::process::id().to_string()),
                    "names the holder: {msg}"
                );
            }
            Ok(_) => panic!("two servers held the lock at once"),
        }

        drop(first);
        assert!(
            super::lock_database(&db).is_ok(),
            "released with its holder"
        );
        let _ = std::fs::remove_file(format!("{db}.lock"));
    }
}
