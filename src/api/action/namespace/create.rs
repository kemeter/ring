use crate::api::dto::namespace::NamespaceOutput;
use crate::api::server::Db;
use crate::models::namespace;
use crate::models::users::User;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct NamespaceInput {
    name: String,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    _user: User,
    Json(input): Json<NamespaceInput>,
) -> impl IntoResponse {
    let utc: DateTime<Utc> = Utc::now();

    let new_namespace = namespace::Namespace {
        id: Uuid::new_v4().to_string(),
        created_at: utc.to_string(),
        updated_at: None,
        name: input.name,
    };

    match namespace::create(&pool, new_namespace.clone()).await {
        Ok(_) => {
            let output = NamespaceOutput::from_to_model(new_namespace);
            (
                StatusCode::CREATED,
                Json(serde_json::to_value(output).unwrap()),
            )
                .into_response()
        }
        Err(e) => {
            if e.to_string().contains("UNIQUE constraint failed") {
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "Namespace with this name already exists"
                    })),
                )
                    .into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": "Failed to create namespace"
                    })),
                )
                    .into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::dto::namespace::NamespaceOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn create_namespace() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "production"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let namespace = response.json::<NamespaceOutput>();
        assert_eq!(namespace.name, "production");
    }

    #[tokio::test]
    async fn create_duplicate_namespace() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "duplicate-ns"
            }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "name": "duplicate-ns"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }
}
