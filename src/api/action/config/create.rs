use crate::api::action::config::validation::{
    CONFIG_DATA_MAX, CONFIG_LABELS_MAX, CONFIG_NAME_MAX, CONFIG_NAME_MIN, CONFIG_NAME_PATTERN,
};
use crate::api::action::namespace::validation::{
    NAMESPACE_NAME_MAX, NAMESPACE_NAME_MIN, NAMESPACE_NAME_PATTERN,
};
use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::api::validation::{ViolationList, problem_response};
use crate::models::audit_log;
use crate::models::config;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct ConfigInput {
    #[validate(
        length(
            min = "NAMESPACE_NAME_MIN",
            max = "NAMESPACE_NAME_MAX",
            code = "config.namespace.length",
            message = "must be 2 to 63 characters"
        ),
        regex(
            path = *NAMESPACE_NAME_PATTERN,
            code = "config.namespace.format",
            message = "must contain only lowercase letters, digits and '-', and start and end with an alphanumeric character"
        )
    )]
    namespace: String,
    #[validate(
        length(
            min = "CONFIG_NAME_MIN",
            max = "CONFIG_NAME_MAX",
            code = "config.name.length",
            message = "must be 1 to 253 characters"
        ),
        regex(
            path = *CONFIG_NAME_PATTERN,
            code = "config.name.format",
            message = "must contain only lowercase letters, digits, '.' and '-', and start and end with an alphanumeric character"
        )
    )]
    name: String,
    #[validate(length(
        min = 1,
        max = "CONFIG_DATA_MAX",
        code = "config.data.length",
        message = "must be 1 to 1048576 bytes (1 MiB)"
    ))]
    data: String,
    #[validate(length(
        max = "CONFIG_LABELS_MAX",
        code = "config.labels.length",
        message = "must be at most 1000 characters"
    ))]
    #[serde(default)]
    labels: Option<String>,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    auth: Auth,
    Json(input): Json<ConfigInput>,
) -> Response {
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return violations.into_response();
    }

    // Scope (`configs:write`) is enforced centrally; the namespace boundary is
    // the body's target namespace.
    if let Err(resp) = require_namespace(&auth.source, &input.namespace) {
        return resp;
    }

    let utc: DateTime<Utc> = Utc::now();
    let new_config = config::Config {
        id: Uuid::new_v4().to_string(),
        created_at: utc.to_string(),
        updated_at: None,
        namespace: input.namespace,
        name: input.name,
        data: input.data,
        labels: input.labels.unwrap_or_default(),
    };

    match config::create(&pool, new_config.clone()).await {
        Ok(_) => {
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "create",
                "config",
                &new_config.name,
                Some(&new_config.namespace),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(serde_json::to_value(new_config).unwrap()),
            )
                .into_response()
        }
        Err(e) if e.to_string().contains("UNIQUE constraint failed") => problem_response(
            StatusCode::CONFLICT,
            "Conflict",
            format!(
                "configuration '{}' already exists in namespace '{}'",
                new_config.name, new_config.namespace
            ),
        ),
        Err(e) => {
            log::error!("Failed to create configuration: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to create configuration",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::dto::config::ConfigOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn create_config() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "namespace": "test",
                "name": "test-config",
                "data": "test data",
                "labels": "test-label"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let config = response.json::<ConfigOutput>();
        assert_eq!(config.namespace, "test");
        assert_eq!(config.name, "test-config");
        assert_eq!(config.data, "test data");
        assert_eq!(config.labels, "test-label");
    }

    #[tokio::test]
    async fn create_duplicate_config_returns_problem_json_conflict() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let payload = json!({
            "namespace": "test",
            "name": "duplicate-config",
            "data": "test data"
        });

        let first = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;
        assert_eq!(first.status_code(), StatusCode::CREATED);

        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;

        assert_eq!(response.status_code(), StatusCode::CONFLICT);
        let ct = response
            .header("content-type")
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.starts_with("application/problem+json"), "got: {}", ct);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["title"], "Conflict");
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("duplicate-config")
        );
    }

    #[tokio::test]
    async fn create_with_missing_fields_returns_422_with_violations() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Missing `namespace` and `data` empty → length violation on data,
        // and an empty namespace fails both length and format.
        let response = server
            .post("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "namespace": "",
                "name": "test-config",
                "data": ""
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["code"].as_str().unwrap().to_string())
            .collect();
        assert!(codes.contains(&"config.namespace.length".to_string()));
        assert!(codes.contains(&"config.data.length".to_string()));
    }
}
