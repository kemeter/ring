use axum::extract::{FromRequestParts, State};
use axum::{Json, response::IntoResponse};
use std::collections::HashMap;

use http::request::Parts;

use serde::Deserialize;

use url::form_urlencoded::parse;

use crate::api::auth::{Auth, filter_by_namespace};
use crate::api::dto::deployment::DeploymentOutput;
use crate::api::server::{Db, RuntimeMap};
use crate::models::deployments;
use http::StatusCode;

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct QueryParameters {
    #[serde(default)]
    namespaces: Vec<String>,
    #[serde(default)]
    status: Vec<String>,
    #[serde(default)]
    kind: Vec<String>,
    /// `key=value` label selectors. A deployment matches only if it carries
    /// every selector. Stored as JSON in the `labels` column, so this is
    /// filtered in memory rather than in SQL.
    #[serde(default)]
    labels: Vec<String>,
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
        let mut kind = Vec::new();
        let mut labels = Vec::new();

        for (key, value) in parsed {
            match key.as_str() {
                "namespace[]" => namespaces.push(value),
                "namespace" => namespaces.push(value),
                "status[]" => status.push(value),
                "status" => status.push(value),
                "kind[]" => kind.push(value),
                "kind" => kind.push(value),
                "label[]" => labels.push(value),
                "label" => labels.push(value),
                _ => {}
            }
        }

        Ok(QueryParameters {
            namespaces,
            status,
            kind,
            labels,
        })
    }
}

pub(crate) async fn list(
    query_parameters: QueryParameters,
    State(pool): State<Db>,
    State(runtimes): State<RuntimeMap>,
    auth: Auth,
) -> axum::response::Response {
    // Scope (`deployments:read`) is enforced centrally. The result set is
    // filtered through the token's namespace boundary below so a
    // namespace-scoped PAT never sees deployments outside its namespaces.
    let mut deployments: Vec<DeploymentOutput> = Vec::new();

    let mut filters: HashMap<String, Vec<String>> = HashMap::new();

    if !query_parameters.namespaces.is_empty() {
        filters.insert(String::from("namespace"), query_parameters.namespaces);
    }

    if !query_parameters.status.is_empty() {
        filters.insert(String::from("status"), query_parameters.status);
    }

    if !query_parameters.kind.is_empty() {
        filters.insert(String::from("kind"), query_parameters.kind);
    }

    let list_deployments = match deployments::find_all(&pool, filters).await {
        Ok(list) => list,
        Err(e) => {
            log::error!("Failed to list deployments: {}", e);
            return Json(deployments).into_response();
        }
    };

    let list_deployments =
        filter_by_namespace(&auth.source, list_deployments, |d| d.namespace.as_str());

    // Labels live as JSON in the `labels` column, so they can't go through the
    // column-based SQL filter — match them in memory. Each selector is `key` or
    // `key=value`; a deployment must satisfy every selector to be kept.
    let label_selectors: Vec<(String, Option<String>)> = query_parameters
        .labels
        .iter()
        .map(|sel| match sel.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (sel.clone(), None),
        })
        .collect();

    let matches_labels = |deployment: &deployments::Deployment| {
        label_selectors.iter().all(|(k, want)| match want {
            Some(v) => deployment.labels.get(k).is_some_and(|got| got == v),
            None => deployment.labels.contains_key(k),
        })
    };

    for deployment in list_deployments.into_iter() {
        if !matches_labels(&deployment) {
            continue;
        }
        let id = deployment.id.clone();
        let runtime_name = deployment.runtime.clone();
        let mut output = DeploymentOutput::from_to_model(deployment);

        if let Some(rt) = runtimes.get(&runtime_name) {
            output.instances = rt.list_instances(id, "running").await;
        }

        deployments.push(output);
    }

    Json(deployments).into_response()
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
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/deployments?status=running")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let deployments = response.json::<Vec<DeploymentOutput>>();
        assert_eq!(2, deployments.len());
    }

    #[tokio::test]
    async fn list_by_namespace() {
        let app = new_test_app().await;
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

    #[tokio::test]
    async fn list_by_kind() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/deployments?kind=worker")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let deployments = response.json::<Vec<DeploymentOutput>>();
        assert!(!deployments.is_empty());
    }
}
