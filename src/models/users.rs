use rusqlite::Connection;
use rusqlite::named_params;
use serde::{Deserialize, Serialize};
use tokio::sync::MutexGuard;
use serde_rusqlite::from_rows;
use serde_rusqlite::from_rows_ref;
use uuid::Uuid;

use crate::serializer::deserialize_null_default;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) id: String,
    pub(crate) created_at: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub(crate) updated_at: Option<String>,
    pub(crate) status: String,
    pub(crate) username: String,
    pub(crate) password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) token: String,
    #[serde(skip_serializing)]
    pub(crate) login_at: Option<String>,
}

pub(crate) fn find(connection: &MutexGuard<Connection>, id: String) -> Result<Option<User>, serde_rusqlite::Error> {
    let mut statement = connection.prepare("SELECT * FROM user WHERE id = :id").unwrap();
    let mut rows = statement.query(named_params!{
        ":id": id,
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
            token = :token,
            login_at = datetime()
        WHERE
            id = :id"
    ).expect("Could not update user");

    statement.execute(named_params!{
        ":token": user.token,
        ":id": user.id,
    }).expect("Could not update user");
}

pub(crate) fn find_by_token(connection: &Connection, token: &str) -> rusqlite::Result<User> {
    let query ="
        SELECT
            id,
            created_at,
            updated_at,
            status,
            username,
            password,
            login_at,
            token
        FROM user
        WHERE
            token = :token";

    let user = connection.query_row(
        query,
        &[(":token", &token)],
        |row| {
            Ok(User {
                id: row.get(0)?,
                created_at: row.get(1)?,
                updated_at: row.get(2)?,
                status: row.get(3)?,
                username: row.get(4)?,
                token: row.get(5)?,
                password: row.get(6)?,
                login_at: row.get(7)?,
            })
        },
    );

    return user;
}

pub(crate) fn find_all(connection: MutexGuard<Connection>) -> Vec<User> {
    let mut statement = connection.prepare("
            SELECT
                id,
                created_at,
                updated_at,
                status,
                username,
                password,
                token,
                login_at
            FROM user"
    ).expect("Could not fetch users");

    let mut users: Vec<User> = Vec::new();
    let mut rows_iter = from_rows::<User>(statement.query([]).unwrap());

    loop {
        match rows_iter.next() {
            None => { break; },
            Some(user) => {
                let user = user.expect("Could not deserialize User item");
                users.push(user);
            }
        }
    }

    return users;
}

pub(crate) fn create(connection: &MutexGuard<Connection>, username: &str, password: &str) {
    let mut statement = connection.prepare("
            INSERT INTO user (
                id,
                created_at,
                status,
                username,
                password,
                token
            ) VALUES (
                :id,
                datetime(),
                :status,
                :username,
                :password,
                :token
            )"
    ).expect("Could not create deployment");

    statement.execute(named_params!{
        ":id": Uuid::new_v4().to_string(),
        ":status": "active",
        ":username": username,
        ":password": password,
        ":token": Uuid::new_v4().to_string()
    }).expect("Could not create user");
}

pub(crate) fn update(connection: &MutexGuard<Connection>, user: &User) {
    let mut statement = connection.prepare("
            UPDATE user
            SET
                username = :username,
                password = :password,
                updated_at = datetime()
            WHERE
                id = :id"
    ).expect("Could not update deployment");

    statement.execute(named_params!{
        ":id": user.id,
        ":username": user.username,
        ":password": user.password
    }).expect("Could not update deployment");
}

pub(crate) fn delete(connection: &MutexGuard<Connection>, user: &User) {
    let mut statement = connection.prepare("
            DELETE FROM user
            WHERE
                id = :id"
    ).expect("Could not delete deployment");

    statement.execute(named_params!{
        ":id": user.id
    }).expect("Could not delete deployment");
}
