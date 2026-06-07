use crate::api::action::namespace::validation::{
    NAMESPACE_NAME_MAX, NAMESPACE_NAME_MIN, NAMESPACE_NAME_PATTERN,
};
use crate::api::action::secret::validation::{
    SECRET_NAME_MAX, SECRET_NAME_MIN, SECRET_NAME_PATTERN, SECRET_VALUE_MAX,
};
use crate::api::auth::{Auth, require_namespace};
use crate::api::server::Db;
use crate::api::validation::{ViolationList, problem_response};
use crate::models::audit_log;
use crate::models::namespace;
use crate::models::secret;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Deserialize, Serialize, Debug, Clone, Validate)]
pub(crate) struct SecretInput {
    #[validate(
        length(
            min = "NAMESPACE_NAME_MIN",
            max = "NAMESPACE_NAME_MAX",
            code = "secret.namespace.length",
            message = "must be 2 to 63 characters"
        ),
        regex(
            path = *NAMESPACE_NAME_PATTERN,
            code = "secret.namespace.format",
            message = "must contain only lowercase letters, digits and '-', and start and end with an alphanumeric character"
        )
    )]
    namespace: String,
    #[validate(
        length(
            min = "SECRET_NAME_MIN",
            max = "SECRET_NAME_MAX",
            code = "secret.name.length",
            message = "must be 2 to 253 characters"
        ),
        regex(
            path = *SECRET_NAME_PATTERN,
            code = "secret.name.format",
            message = "must contain only letters, digits, '_', '.' and '-', and start and end with an alphanumeric character"
        )
    )]
    name: String,
    #[validate(length(
        min = 1,
        max = "SECRET_VALUE_MAX",
        code = "secret.value.length",
        message = "must be 1 to 1048576 bytes (1 MiB)"
    ))]
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
    auth: Auth,
    Json(input): Json<SecretInput>,
) -> Response {
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return violations.into_response();
    }

    // Scope (`secrets:write`) is enforced centrally by the auth middleware.
    // The namespace boundary is the body's target namespace: a namespace-scoped
    // PAT may only create secrets in a namespace it is scoped to.
    if let Err(resp) = require_namespace(&auth.source, &input.namespace) {
        return resp;
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
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "create",
                "secret",
                &new_secret.name,
                Some(&new_secret.namespace),
            )
            .await;
            let output = SecretOutput {
                id: new_secret.id,
                created_at: new_secret.created_at,
                namespace: new_secret.namespace,
                name: new_secret.name,
            };
            (StatusCode::CREATED, Json(output)).into_response()
        }
        Err(e) if e.to_string().contains("UNIQUE constraint failed") => problem_response(
            StatusCode::CONFLICT,
            "Conflict",
            format!(
                "secret '{}' already exists in namespace '{}'",
                new_secret.name, new_secret.namespace
            ),
        ),
        Err(e) => {
            error!("Failed to create secret: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to create secret",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

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
    async fn create_secret_with_uppercase_env_style_name() {
        // Secret names mirror the env-variable keys consumers inject
        // (SCREAMING_SNAKE_CASE), so uppercase and underscore must be
        // accepted, otherwise such pushes 422 and namespaces stay empty.
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "alpacode").await;

        let response = server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "alpacode",
                "name": "POSTGRESQL_ADDON_PASSWORD",
                "value": "s3cret"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let body: serde_json::Value = response.json();
        assert_eq!(body["name"], "POSTGRESQL_ADDON_PASSWORD");
        assert_eq!(body["namespace"], "alpacode");
    }

    #[tokio::test]
    async fn create_secret_with_underscore_boundary_is_rejected() {
        // Underscore is allowed *inside* the name but the value must still
        // start and end with an alphanumeric character: `_FOO` and `FOO_`
        // must 422 so the regex anchors can't silently regress.
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        create_namespace(&server, &token, "alpacode").await;

        for invalid_name in ["_LEADING_UNDERSCORE", "TRAILING_UNDERSCORE_"] {
            let response = server
                .post("/secrets")
                .add_header("Authorization", format!("Bearer {}", token))
                .json(&serde_json::json!({
                    "namespace": "alpacode",
                    "name": invalid_name,
                    "value": "s3cret"
                }))
                .await;

            assert_eq!(
                response.status_code(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "expected `{}` to be rejected",
                invalid_name
            );
            let body: serde_json::Value = response.json();
            assert!(
                body["violations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v["code"] == "secret.name.format"),
                "expected a secret.name.format violation for `{}`, got {}",
                invalid_name,
                body
            );
        }
    }

    #[tokio::test]
    async fn create_secret_in_nonexistent_namespace_returns_problem_json() {
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
        let ct = response
            .header("content-type")
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.starts_with("application/problem+json"), "got: {}", ct);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["status"], 404);
        assert_eq!(body["title"], "Not Found");
        assert!(body["detail"].as_str().unwrap().contains("does-not-exist"));
    }

    #[tokio::test]
    async fn create_duplicate_secret_returns_problem_json_conflict() {
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
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["title"], "Conflict");
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("already exists in namespace")
        );
    }

    #[tokio::test]
    async fn create_with_invalid_fields_returns_422_with_violations() {
        set_test_key();
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Namespace and name both invalid (uppercase / leading dash), value empty.
        let response = server
            .post("/secrets")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "namespace": "Production",
                "name": "-bad",
                "value": ""
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
        assert!(codes.contains(&"secret.namespace.format".to_string()));
        assert!(codes.contains(&"secret.name.format".to_string()));
        assert!(codes.contains(&"secret.value.length".to_string()));
    }
}
