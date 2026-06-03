use axum::extract::{FromRequestParts, State};
use axum::{Json, response::IntoResponse};
use std::collections::HashMap;

use http::request::Parts;

use serde::Deserialize;

use crate::api::auth::{Auth, filter_by_namespace};
use crate::api::dto::config::ConfigOutput;
use crate::api::server::Db;
use crate::models::config as ConfigModel;
use axum::response::Response;
use http::StatusCode;
use url::form_urlencoded::parse;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct QueryParameters {
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
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
        let mut names = Vec::new();

        for (key, value) in parsed {
            match key.as_str() {
                "namespace[]" => namespaces.push(value),
                "namespace" => namespaces.push(value),
                "name[]" => names.push(value),
                "name" => names.push(value),
                _ => {}
            }
        }

        Ok(QueryParameters { namespaces, names })
    }
}

pub(crate) async fn list(
    query_parameters: QueryParameters,
    State(pool): State<Db>,
    auth: Auth,
) -> Response {
    // Scope (`configs:read`) is enforced centrally. The result is filtered
    // through the token's namespace boundary so a namespace-scoped PAT never
    // sees configs outside its namespaces, regardless of the `?namespace=`
    // filter it supplies.
    let mut configs: Vec<ConfigOutput> = Vec::new();

    let mut filters: HashMap<String, Vec<String>> = HashMap::new();

    if !query_parameters.namespaces.is_empty() {
        filters.insert(String::from("namespace"), query_parameters.namespaces);
    }

    if !query_parameters.names.is_empty() {
        filters.insert(String::from("name"), query_parameters.names);
    }

    let list_configs = match ConfigModel::find_all(&pool, filters).await {
        Ok(list) => list,
        Err(e) => {
            log::error!("Failed to list configs: {}", e);
            return Json(configs).into_response();
        }
    };

    let list_configs = filter_by_namespace(&auth.source, list_configs, |c| c.namespace.as_str());

    for config in list_configs.into_iter() {
        let output = ConfigOutput::from_to_model(config.clone());
        configs.push(output);
    }

    Json(configs).into_response()
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
        let app = new_test_app().await;
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
        let app = new_test_app().await;
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
        let app = new_test_app().await;
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
        let app = new_test_app().await;
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
        let app = new_test_app().await;
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

    #[tokio::test]
    async fn list_configs_filter_by_name() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?name=app.properties")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(2, configs.len()); // production + staging both have app.properties
        for config in configs {
            assert_eq!(config.name, "app.properties");
        }
    }

    #[tokio::test]
    async fn list_configs_filter_by_namespace_and_name() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?namespace=staging&name=app.properties")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(1, configs.len());
        assert_eq!(configs[0].namespace, "staging");
        assert_eq!(configs[0].name, "app.properties");
    }

    #[tokio::test]
    async fn list_configs_filter_by_nonexistent_name() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs?name=nonexistent")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(0, configs.len());
    }
}
