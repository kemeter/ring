pub mod users;
pub mod deployments;
pub mod configs;
pub mod events;

use sqlx::SqlitePool;

pub async fn load_all_fixtures(pool: &SqlitePool) {
    users::load(pool).await;
    deployments::load(pool).await;
    configs::load(pool).await;
    events::load(pool).await;
}
