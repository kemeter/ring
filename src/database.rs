use rusqlite::Connection;
use rusqlite::OpenFlags;
use std::env;

pub(crate) fn get_database_connection() -> Connection {
    let mut db_flags = OpenFlags::empty();

    db_flags.insert(OpenFlags::SQLITE_OPEN_READ_WRITE);
    db_flags.insert(OpenFlags::SQLITE_OPEN_CREATE);
    db_flags.insert(OpenFlags::SQLITE_OPEN_FULL_MUTEX);
    db_flags.insert(OpenFlags::SQLITE_OPEN_NOFOLLOW);
    db_flags.insert(OpenFlags::SQLITE_OPEN_PRIVATE_CACHE);

    let database_file_path = env::var_os("RING_DATABASE_PATH").unwrap_or_else(|| "ring.db".into());

    let conn = Connection::open_with_flags(database_file_path, db_flags).expect("Could not open database");

    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
        .expect("Could not set WAL mode");

    conn
}