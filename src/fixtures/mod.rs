pub mod users;
pub mod deployments;
pub mod configs;
pub mod events;

use rusqlite::Connection;

pub fn load_all_fixtures(connection: &mut Connection) {
    users::load(connection);
    deployments::load(connection);
    configs::load(connection);
    events::load(connection);
}