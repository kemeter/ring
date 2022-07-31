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

    let database_file_path = match env::var_os("RING_DATABASE_PATH") {
        Some(variable) => variable.into_string().unwrap(),
        None => String::from("ring.db")
    };

    Connection::open_with_flags(database_file_path, db_flags).expect("Could not test: DB not created")
}