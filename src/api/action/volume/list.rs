use crate::api::server::Db;
use crate::models::users::User;
use crate::models::volumes;
use axum::extract::{FromRequestParts, State};
use axum::{Json, response::IntoResponse};
use http::StatusCode;
use http::request::Parts;
use serde::Serialize;
use std::collections::HashMap;
use url::form_urlencoded::parse;

#[derive(Debug, Clone)]
pub(crate) struct QueryParameters {
    namespaces: Vec<String>,
}

impl<S> FromRequestParts<S> for QueryParameters
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, axum::Json<serde_json::Value>);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let request_uri = parts.uri.clone();
        let query = request_uri.query().unwrap_or("");
        let parsed: Vec<(String, String)> = parse(query.as_bytes()).into_owned().collect();

        let mut namespaces = Vec::new();
        for (key, value) in parsed {
            match key.as_str() {
                "namespace[]" | "namespace" => namespaces.push(value),
                _ => {}
            }
        }

        Ok(QueryParameters { namespaces })
    }
}

#[derive(Serialize)]
struct VolumeOutput {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    namespace: String,
    name: String,
    size: Option<i64>,
    backend_type: String,
    host_path: String,
    labels: HashMap<String, String>,
}

pub(crate) async fn list(
    query_parameters: QueryParameters,
    State(pool): State<Db>,
    _user: User,
) -> impl IntoResponse {
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();
    if !query_parameters.namespaces.is_empty() {
        filters.insert(String::from("namespace"), query_parameters.namespaces);
    }

    match volumes::find_all(&pool, filters).await {
        Ok(list) => {
            let output: Vec<VolumeOutput> = list
                .into_iter()
                .map(|volume| VolumeOutput {
                    labels: volume.labels_map(),
                    id: volume.id,
                    created_at: volume.created_at,
                    updated_at: volume.updated_at,
                    namespace: volume.namespace,
                    name: volume.name,
                    size: volume.size,
                    backend_type: volume.backend_type,
                    host_path: volume.host_path,
                })
                .collect();
            (StatusCode::OK, Json(output)).into_response()
        }
        Err(e) => {
            error!("Failed to list volumes: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Failed to list volumes" })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::{login, new_test_app};
    use axum::http::StatusCode;
    use axum_test::TestServer;
    use serde_json::json;

    #[tokio::test]
    async fn list_returns_created_volumes() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "name": "production" }))
            .await;
        server
            .post("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&json!({ "namespace": "production", "name": "db-data" }))
            .await;

        let response = server
            .get("/volumes")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let body: serde_json::Value = response.json();
        let arr = body.as_array().unwrap();
        assert!(arr.iter().any(|v| v["name"] == "db-data"));
    }
}
