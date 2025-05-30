use axum::extract::State;
use axum::Json;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;
use crate::api::server::Db;
use crate::models::config;
use crate::models::users::User;

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct ConfigInput {
    namespace: String,
    name: String,
    data: String,
    #[serde(default)]
    labels: Option<String>,
}

impl ConfigInput {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        let errors = validator::ValidationErrors::new();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

pub(crate) async fn create(
    State(connexion): State<Db>,
    _user: User,
    Json(input): Json<ConfigInput>,
) -> impl IntoResponse {
    match input.validate() {
        Ok(_) => {
            let guard = connexion.lock().await;
            let utc: DateTime<Utc> = Utc::now();

            let config = config::Config {
                id: Uuid::new_v4().to_string(),
                created_at: utc.to_string(),
                updated_at: None,
                namespace: input.namespace,
                name: input.name,
                data: input.data,
                labels: input.labels.unwrap_or_default(),
            };

            let _ = config::create(&guard, config);

        },
        Err(_) => (),
    }
}