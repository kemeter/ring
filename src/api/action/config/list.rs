use axum::extract::{FromRequestParts, State};
use axum::{response::IntoResponse, Json};
use std::collections::HashMap;

use http::request::Parts;

use serde::Deserialize;

use crate::api::dto::config::ConfigOutput;
use crate::api::server::Db;
use http::StatusCode;
use crate::models::config as ConfigModel;
use crate::models::users::User;
use url::form_urlencoded::parse;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct QueryParameters {
    #[serde(default)]
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
                "namespace[]" => namespaces.push(value),
                "namespace" => namespaces.push(value),
                _ => {}
            }
        }

        Ok(QueryParameters{
            namespaces,
        })
    }
}

pub(crate) async fn list(
    query_parameters: QueryParameters,
    State(connexion): State<Db>,
    _user: User
) -> impl IntoResponse {

    let mut configs: Vec<ConfigOutput> = Vec::new();

    let list_configs = {
        let guard = connexion.lock().await;
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();

        if !query_parameters.namespaces.is_empty() {
            filters.insert(String::from("namespace"), query_parameters.namespaces);
        }

        ConfigModel::find_all(&guard, filters)
    };

    for config in list_configs.into_iter() {
        let output = ConfigOutput::from_to_model(config.clone());
        configs.push(output);
    }

    Json(configs)
}

#[cfg(test)]
mod tests {
    use crate::api::dto::config::ConfigOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn list_all_configs() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(4, configs.len()); // Should have 4 configs from fixtures
    }

    #[tokio::test]
    async fn list_configs_filter_by_namespace_production() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?namespace=production")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(2, configs.len()); // Should have 2 configs in production namespace
        
        // Verify all configs are in production namespace
        for config in configs {
            assert_eq!(config.namespace, "production");
        }
    }

    #[tokio::test]
    async fn list_configs_filter_by_namespace_staging() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?namespace=staging")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(1, configs.len()); // Should have 1 config in staging namespace
        assert_eq!(configs[0].namespace, "staging");
        assert_eq!(configs[0].name, "app.properties");
    }

    #[tokio::test]
    async fn list_configs_filter_by_multiple_namespaces() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?namespace=kemeter&namespace=staging")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(2, configs.len()); // Should have 2 configs (1 from kemeter + 1 from staging)
        
        // Verify configs are from the right namespaces
        let namespaces: Vec<String> = configs.iter().map(|c| c.namespace.clone()).collect();
        assert!(namespaces.contains(&"kemeter".to_string()));
        assert!(namespaces.contains(&"staging".to_string()));
    }

    #[tokio::test]
    async fn list_configs_filter_by_nonexistent_namespace() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?namespace=nonexistent")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(0, configs.len()); // Should have no configs
    }
}
