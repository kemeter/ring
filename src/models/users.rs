use rusqlite::Connection;
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use serde_rusqlite::from_rows;
use serde_rusqlite::from_rows_ref;
use std::sync::MutexGuard;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) status: String,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) token: String,
    #[serde(skip_deserializing)]
    #[serde(skip_serializing)]
    pub(crate) login_at: i64
}

pub(crate) fn find_by_token(connection: &Connection, token: &str) -> Result<Option<User>, serde_rusqlite::Error> {

    let mut statement = connection.prepare("SELECT * FROM user WHERE token = :token").unwrap();
    let mut rows = statement.query(named_params!{
        ":token": token
    }).unwrap();

    let mut ref_rows = from_rows_ref::<User>(&mut rows);
    let result = ref_rows.next();

    result.transpose()
}

pub(crate) fn find_by_username(connection: &MutexGuard<Connection>, username: &str) -> Result<Option<User>, serde_rusqlite::Error> {
    let mut statement = connection.prepare("SELECT * FROM user WHERE username = :username").unwrap();
    let mut rows = statement.query(named_params!{
        ":username": username,
    }).unwrap();


    let mut ref_rows = from_rows_ref::<User>(&mut rows);
    let result = ref_rows.next();

    result.transpose()
}

pub(crate) fn login(connection: &MutexGuard<Connection>, user: User) {
    let mut statement = connection.prepare("
        UPDATE user
        SET
            login_at = date('now')
        WHERE
            id = :id"
    ).expect("Could not update user");

    statement.execute(named_params!{
        ":id": user.id,
    }).expect("Could not update user");
}
