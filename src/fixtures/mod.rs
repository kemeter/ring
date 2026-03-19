pub mod configs;
pub mod deployments;
pub mod events;
pub mod users;

use sqlx::SqlitePool;

pub async fn load_all_fixtures(pool: &SqlitePool) {
    users::load(pool).await;
    deployments::load(pool).await;
    configs::load(pool).await;
    events::load(pool).await;
}
