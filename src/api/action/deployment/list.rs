use axum::extract::{FromRequestParts, State};
use axum::{response::IntoResponse, Json};
use std::collections::HashMap;

use http::request::Parts;

use serde::Deserialize;

use url::form_urlencoded::parse;

use crate::api::dto::deployment::DeploymentOutput;
use crate::api::server::Db;
use crate::models::deployments;
use crate::models::users::User;
use crate::runtime::docker;
use http::StatusCode;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct QueryParameters {
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    status: Vec<String>,
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

        let mut status = Vec::new();
        let mut namespaces = Vec::new();

        for (key, value) in parsed {
            match key.as_str() {
                "namespace[]" => namespaces.push(value),
                "namespace" => namespaces.push(value),
                "status[]" => status.push(value),
                "status" => status.push(value),
                _ => {}
            }
        }

        Ok(QueryParameters{
            namespaces,
            status
        })
    }
}

pub(crate) async fn list(
    query_parameters: QueryParameters,
    State(connexion): State<Db>,
    _user: User
) -> impl IntoResponse {

    let mut deployments: Vec<DeploymentOutput> = Vec::new();

    let list_deployments = {
        let guard = connexion.lock().await;
        let mut filters: HashMap<String, Vec<String>> = HashMap::new();

        if !query_parameters.namespaces.is_empty() {
            filters.insert(String::from("namespace"), query_parameters.namespaces);
        }

        if !query_parameters.status.is_empty() {
            filters.insert(String::from("status"), query_parameters.status);
        }

        deployments::find_all(&guard, filters)
    };

    for deployment in list_deployments.into_iter() {

        let mut output = DeploymentOutput::from_to_model(deployment.clone());
        let instances = docker::list_instances(deployment.id.to_string(), "running").await;
        output.instances = instances;

        deployments.push(output);
    }

    Json(deployments)
}

#[cfg(test)]
mod tests {
    use crate::api::dto::deployment::DeploymentOutput;
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
            .get("/deployments?status=running")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let deployments = response.json::<Vec<DeploymentOutput>>();
        assert_eq!(1, deployments.len());
    }


    #[tokio::test]
    async fn list_by_namespace() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/deployments?namespace=kemeter")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let deployments = response.json::<Vec<DeploymentOutput>>();
        assert_eq!(1, deployments.len());
    }
}
