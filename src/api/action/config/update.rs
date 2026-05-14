use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::{Json, http::StatusCode};
use serde::Deserialize;
use validator::Validate;

use crate::api::action::config::validation::{
    CONFIG_DATA_MAX, CONFIG_LABELS_MAX, CONFIG_NAME_MAX, CONFIG_NAME_MIN, CONFIG_NAME_PATTERN,
};
use crate::api::dto::config::ConfigOutput;
use crate::api::server::Db;
use crate::api::validation::{Violation, ViolationList, problem_response};
use crate::models::config as ConfigModel;
use crate::models::users::User;

#[derive(Deserialize, Debug, Validate)]
pub(crate) struct UpdateConfigRequest {
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
    pub name: String,

    #[validate(length(
        min = 1,
        max = "CONFIG_DATA_MAX",
        code = "config.data.length",
        message = "must be 1 to 1048576 bytes (1 MiB)"
    ))]
    pub data: String,

    #[validate(length(
        max = "CONFIG_LABELS_MAX",
        code = "config.labels.length",
        message = "must be at most 1000 characters"
    ))]
    #[serde(default)]
    pub labels: Option<String>,
}

pub(crate) async fn update(
    Path(id): Path<String>,
    State(pool): State<Db>,
    _user: User,
    Json(request): Json<UpdateConfigRequest>,
) -> Response {
    let mut violations: ViolationList = match request.validate() {
        Ok(()) => ViolationList::new(),
        Err(errs) => errs.into(),
    };

    // JSON-shape rule for `data`: the field must round-trip as a JSON
    // value. We add this as a regular violation so it shows up alongside
    // any length/format failures rather than overriding them with a
    // single 400.
    if !request.data.is_empty() && serde_json::from_str::<serde_json::Value>(&request.data).is_err()
    {
        violations.push(Violation::new(
            "data",
            "must be valid JSON",
            "config.data.invalid_json",
        ));
    }

    if !violations.is_empty() {
        return violations.into_response();
    }

    match ConfigModel::find(&pool, &id).await {
        Ok(Some(mut config)) => {
            config.name = request.name;
            config.data = request.data;
            config.labels = request.labels.unwrap_or_default();
            config.updated_at = Some(chrono::Utc::now().to_rfc3339());

            match ConfigModel::update(&pool, config.clone()).await {
                Ok(_) => {
                    let output = ConfigOutput::from_to_model(config);
                    (StatusCode::OK, Json(output)).into_response()
                }
                Err(_) => problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "failed to update configuration",
                ),
            }
        }
        Ok(None) => problem_response(
            StatusCode::NOT_FOUND,
            "Not Found",
            format!("configuration '{}' does not exist", id),
        ),
        Err(_) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "database error",
        ),
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
    async fn update_config_name() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "updated-config-name",
                "data": "{\"key\": \"value\"}"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let config = response.json::<ConfigOutput>();
        assert_eq!(config.name, "updated-config-name");
        assert_eq!(config.data, "{\"key\": \"value\"}");
        assert!(config.updated_at.is_some());
    }

    #[tokio::test]
    async fn update_config_invalid_json_data_returns_422_violation() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "my-config",
                "data": "invalid json"
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
        assert!(codes.contains(&"config.data.invalid_json".to_string()));
    }

    #[tokio::test]
    async fn update_nonexistent_config_returns_problem_json_not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .put("/configs/nonexistent")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "new-name",
                "data": "{\"test\": true}"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["title"], "Not Found");
        assert!(body["detail"].as_str().unwrap().contains("nonexistent"));
    }

    #[tokio::test]
    async fn update_config_multiple_fields() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        let response = server
            .put("/configs/cde7806a-21af-473b-968b-08addc7bf0ba")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({
                "name": "multi-update",
                "data": "{\"env\": \"production\"}",
                "labels": "{\"team\": \"backend\"}"
            }))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let config = response.json::<ConfigOutput>();
        assert_eq!(config.name, "multi-update");
        assert_eq!(config.data, "{\"env\": \"production\"}");
        assert_eq!(config.labels, "{\"team\": \"backend\"}");
    }
}
