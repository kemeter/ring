use crate::api::action::namespace::validation::{
    NAMESPACE_NAME_MAX, NAMESPACE_NAME_MIN, NAMESPACE_NAME_PATTERN,
};
use crate::api::auth::Auth;
use crate::api::dto::namespace::NamespaceOutput;
use crate::api::server::Db;
use crate::api::validation::{ViolationList, problem_response};
use crate::models::audit_log;
use crate::models::namespace;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;
use validator::Validate;

#[derive(Deserialize, Debug, Clone, Validate)]
pub(crate) struct NamespaceInput {
    #[validate(
        length(
            min = "NAMESPACE_NAME_MIN",
            max = "NAMESPACE_NAME_MAX",
            code = "namespace.name.length",
            message = "must be 2 to 63 characters"
        ),
        regex(
            path = *NAMESPACE_NAME_PATTERN,
            code = "namespace.name.format",
            message = "must contain only lowercase letters, digits and '-', and start and end with an alphanumeric character"
        )
    )]
    name: String,
}

pub(crate) async fn create(
    State(pool): State<Db>,
    auth: Auth,
    Json(input): Json<NamespaceInput>,
) -> Response {
    if let Err(errs) = input.validate() {
        let violations: ViolationList = errs.into();
        return violations.into_response();
    }

    // Scope (`namespaces:write`) is enforced centrally by the auth middleware.
    let utc: DateTime<Utc> = Utc::now();
    let new_namespace = namespace::Namespace {
        id: Uuid::new_v4().to_string(),
        created_at: utc.to_string(),
        updated_at: None,
        name: input.name,
    };

    match namespace::create(&pool, new_namespace.clone()).await {
        Ok(_) => {
            // A namespace's own creation is recorded under its own name so it
            // shows up in `ring namespace audit <ns>` for that namespace.
            let _ = audit_log::record(
                &pool,
                Some(&auth.user.id),
                "create",
                "namespace",
                &new_namespace.name,
                Some(&new_namespace.name),
            )
            .await;
            let output = NamespaceOutput::from_to_model(new_namespace);
            (
                StatusCode::CREATED,
                Json(serde_json::to_value(output).unwrap()),
            )
                .into_response()
        }
        Err(e) if e.to_string().contains("UNIQUE constraint failed") => problem_response(
            StatusCode::CONFLICT,
            "Conflict",
            format!("namespace '{}' already exists", new_namespace.name),
        ),
        Err(e) => {
            log::error!("Failed to create namespace: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to create namespace",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::dto::namespace::NamespaceOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::{new_test_app, new_test_app_with_pool};
    use crate::models::audit_log;
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn create_namespace() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "production" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::CREATED);
        let namespace = response.json::<NamespaceOutput>();
        assert_eq!(namespace.name, "production");
    }

    #[tokio::test]
    async fn create_namespace_writes_an_audit_entry() {
        let (pool, app) = new_test_app_with_pool().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "production" }))
            .await;
        assert_eq!(response.status_code(), StatusCode::CREATED);

        // The write must have produced a namespace-scoped audit entry naming
        // the action, target and author.
        let entries = audit_log::find_by_namespace(&pool, "production", None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "create");
        assert_eq!(entries[0].target_type, "namespace");
        assert_eq!(entries[0].target_name, "production");
        assert!(
            entries[0].user_id.is_some(),
            "audit entry must carry the authenticated user's id"
        );
    }

    #[tokio::test]
    async fn create_duplicate_namespace_returns_problem_json_conflict() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let payload = json!({ "name": "duplicate-ns" });
        let first = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .await;
        assert_eq!(first.status_code(), StatusCode::CREATED);

        let response = server
            .post("/namespaces")
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
        assert_eq!(body["status"], 409);
        assert_eq!(body["title"], "Conflict");
        assert!(body["detail"].as_str().unwrap().contains("duplicate-ns"));
    }

    #[tokio::test]
    async fn create_with_short_name_returns_422_problem_json() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "a" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let v = &body["violations"];
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["property_path"], "name");
        assert_eq!(v[0]["code"], "namespace.name.length");
    }

    #[tokio::test]
    async fn create_with_uppercase_name_trips_format() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "Production" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["code"].as_str().unwrap().to_string())
            .collect();
        assert!(codes.contains(&"namespace.name.format".to_string()));
    }

    #[tokio::test]
    async fn create_accumulates_all_violations() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // `-` fails length (1 char) AND format (leading dash).
        let response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "-" }))
            .await;

        assert_eq!(response.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = response.json::<serde_json::Value>();
        let codes: Vec<String> = body["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["code"].as_str().unwrap().to_string())
            .collect();
        assert!(codes.contains(&"namespace.name.length".to_string()));
        assert!(codes.contains(&"namespace.name.format".to_string()));
    }
}
