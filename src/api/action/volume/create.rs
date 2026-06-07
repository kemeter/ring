use crate::api::action::namespace::validation::{
    NAMESPACE_NAME_MAX, NAMESPACE_NAME_MIN, NAMESPACE_NAME_PATTERN,
};
use crate::api::action::volume::validation::{
    VOLUME_NAME_MAX, VOLUME_NAME_MIN, VOLUME_NAME_PATTERN,
};
use crate::api::server::Db;
use crate::api::validation::{ViolationList, problem_response};
use crate::models::audit_log;
use crate::models::namespace;
use crate::models::users::User;
use crate::models::volumes;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use validator::Validate;

/// Backends a volume can be provisioned on. `local` is the Docker named-volume
/// driver; `directory` is the Cloud Hypervisor virtiofs directory backend.
const ALLOWED_BACKENDS: &[&str] = &["local", "directory"];

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct VolumeInput {
    #[validate(
        length(
            min = "NAMESPACE_NAME_MIN",
            max = "NAMESPACE_NAME_MAX",
            code = "volume.namespace.length",
            message = "must be 2 to 63 characters"
        ),
        regex(
            path = *NAMESPACE_NAME_PATTERN,
            code = "volume.namespace.format",
            message = "must contain only lowercase letters, digits and '-', and start and end with an alphanumeric character"
        )
    )]
    namespace: String,
    #[validate(
        length(
            min = "VOLUME_NAME_MIN",
            max = "VOLUME_NAME_MAX",
            code = "volume.name.length",
            message = "must be 2 to 253 characters"
        ),
        regex(
            path = *VOLUME_NAME_PATTERN,
            code = "volume.name.format",
            message = "must contain only lowercase letters, digits, '_', '.' and '-', and start and end with an alphanumeric character"
        )
    )]
    name: String,
    /// Optional size hint in bytes. Not enforced as a quota today.
    #[serde(default)]
    size: Option<i64>,
    /// Storage backend. Defaults to `local` when omitted.
    #[serde(default)]
    backend_type: Option<String>,
    #[serde(default)]
    labels: HashMap<String, String>,
}

#[derive(Serialize)]
struct VolumeOutput {
    id: String,
    created_at: String,
    namespace: String,
    name: String,
    backend_type: String,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    user: User,
    Json(input): Json<VolumeInput>,
) -> Response {
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return violations.into_response();
    }

    let backend_type = input.backend_type.unwrap_or_else(|| "local".to_string());
    if !ALLOWED_BACKENDS.contains(&backend_type.as_str()) {
        return problem_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Unprocessable Entity",
            format!(
                "backend_type '{}' is not supported (expected one of: {})",
                backend_type,
                ALLOWED_BACKENDS.join(", ")
            ),
        );
    }

    match namespace::find_by_name(&pool, &input.namespace).await {
        Ok(None) => {
            return problem_response(
                StatusCode::NOT_FOUND,
                "Not Found",
                format!("namespace '{}' does not exist", input.namespace),
            );
        }
        Err(e) => {
            error!("Failed to check namespace: {}", e);
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to verify namespace",
            );
        }
        Ok(Some(_)) => {}
    }

    // host_path is the Docker volume name / directory name once provisioned;
    // it equals the user-facing name for the local + directory backends.
    let new_volume = volumes::Volume::create(
        input.name.clone(),
        input.namespace.clone(),
        input.size,
        backend_type,
        input.name.clone(),
        input.labels,
    );

    match volumes::insert(&pool, &new_volume).await {
        Ok(_) => {
            let _ = audit_log::record(
                &pool,
                Some(&user.id),
                "create",
                "volume",
                &new_volume.name,
                Some(&new_volume.namespace),
            )
            .await;
            let output = VolumeOutput {
                id: new_volume.id,
                created_at: new_volume.created_at,
                namespace: new_volume.namespace,
                name: new_volume.name,
                backend_type: new_volume.backend_type,
            };
            (StatusCode::CREATED, Json(output)).into_response()
        }
        // Use sqlx's typed constraint classification rather than matching on the
        // error message string, which varies across SQLite builds — same approach
        // as `volumes::register_if_absent`.
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => problem_response(
            StatusCode::CONFLICT,
            "Conflict",
            format!(
                "volume '{}' already exists in namespace '{}'",
                new_volume.name, new_volume.namespace
            ),
        ),
        Err(e) => {
            error!("Failed to create volume: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to create volume",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    async fn create_namespace(server: &TestServer, token: &str, name: &str) {
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": name }))
            .await;
    }

    #[tokio::test]
    async fn create_volume() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "production").await;

        let response = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "namespace": "production",
                "name": "db-data"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        assert_eq!(body["name"], "db-data");
        assert_eq!(body["namespace"], "production");
        assert_eq!(body["backend_type"], "local");
    }

    #[tokio::test]
    async fn create_volume_in_nonexistent_namespace_returns_404() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "namespace": "ghost", "name": "db-data" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_duplicate_volume_returns_conflict() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "staging").await;
        let payload = json!({ "namespace": "staging", "name": "cache-vol" });

        server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;

        let response = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn create_volume_rejects_unknown_backend() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "production").await;

        let response = server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "namespace": "production",
                "name": "weird-vol",
                "backend_type": "s3"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
