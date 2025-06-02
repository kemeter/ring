use axum::extract::{FromRequestParts, Query, State};
use axum::{response::IntoResponse, Json};
use std::collections::HashMap;

use http::request::Parts;

use serde::Deserialize;

use crate::api::dto::config::ConfigOutput;
use crate::api::server::{AuthError, Db};
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
    type Rejection = AuthError;

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
    Query(query_parameters): Query<QueryParameters>,
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
    async fn list() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/configs")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let configs = response.json::<Vec<ConfigOutput>>();
        assert_eq!(1, configs.len());
    }
}
