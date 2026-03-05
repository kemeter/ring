use axum::extract::State;
use axum::Json;
use axum::response::IntoResponse;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::api::server::Db;
use crate::models::secret;
use crate::models::namespace;
use crate::models::users::User;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct SecretInput {
    namespace: String,
    name: String,
    value: String,
}

#[derive(Serialize)]
struct SecretOutput {
    id: String,
    created_at: String,
    namespace: String,
    name: String,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    _user: User,
    Json(input): Json<SecretInput>,
) -> impl IntoResponse {
    match namespace::find_by_name(&pool, &input.namespace).await {
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": format!("Namespace '{}' not found", input.namespace)
            }))).into_response();
        }
        Err(e) => {
            log::error!("Failed to check namespace: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": "Failed to verify namespace"
            }))).into_response();
        }
        Ok(Some(_)) => {}
    }

    let encrypted_value = secret::encrypt_value(&input.value);

    let new_secret = secret::Secret {
        id: Uuid::new_v4().to_string(),
        created_at: Utc::now().to_string(),
        updated_at: None,
        namespace: input.namespace,
        name: input.name,
        value: encrypted_value,
    };

    match secret::create(&pool, &new_secret).await {
        Ok(_) => {
            let output = SecretOutput {
                id: new_secret.id,
                created_at: new_secret.created_at,
                namespace: new_secret.namespace,
                name: new_secret.name,
            };
            (StatusCode::CREATED, Json(output)).into_response()
        }
        Err(e) => {
            if e.to_string().contains("UNIQUE constraint failed") {
                (StatusCode::CONFLICT, Json(serde_json::json!({
                    "error": "Secret with this name already exists in this namespace"
                }))).into_response()
            } else {
                log::error!("Failed to create secret: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                    "error": "Failed to create secret"
                }))).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use axum::http::StatusCode;
    use crate::api::server::tests::new_test_app;
    use crate::api::server::tests::login;

    fn set_test_key() {
        use base64::Engine;
        let key = [0u8; 32];
        let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
        unsafe { std::env::set_var("RING_SECRET_KEY", key_b64) };
    }

    async fn create_namespace(server: &TestServer, token: &str, name: &str) {
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({ "name": name }))
            .await;
    }

    #[tokio::test]
    async fn create_secret() {
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "production").await;

        let response = server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "production",
                "name": "db-password",
                "value": "super-secret"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        assert_eq!(body["name"], "db-password");
        assert_eq!(body["namespace"], "production");
    }

    #[tokio::test]
    async fn create_secret_in_nonexistent_namespace() {
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "does-not-exist",
                "name": "db-password",
                "value": "super-secret"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_duplicate_secret() {
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "staging").await;

        let payload = serde_json::json!({
            "namespace": "staging",
            "name": "api-key",
            "value": "secret-value"
        });

        server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;

        let response = server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }
}
