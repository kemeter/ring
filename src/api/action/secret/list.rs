use crate::api::auth::{Auth, filter_by_namespace};
use crate::api::server::Db;
use crate::models::secret as SecretModel;
use axum::extract::{FromRequestParts, State};
use axum::response::Response;
use axum::{Json, response::IntoResponse};
use http::StatusCode;
use http::request::Parts;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

        Ok(QueryParameters { namespaces })
    }
}

#[derive(Serialize)]
struct SecretOutput {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    namespace: String,
    name: String,
}

pub(crate) async fn list(
    query_parameters: QueryParameters,
    State(pool): State<Db>,
    auth: Auth,
) -> Response {
    // Scope (`secrets:read`) is enforced centrally. The `?namespace=` filter is
    // a caller convenience, not a security boundary: a namespace-scoped PAT
    // must never see other namespaces' secrets even if it omits the filter, so
    // the result set is filtered through the token's namespace boundary below.
    let mut filters: HashMap<String, Vec<String>> = HashMap::new();

    if !query_parameters.namespaces.is_empty() {
        filters.insert(String::from("namespace"), query_parameters.namespaces);
    }

    let secrets = match SecretModel::find_all(&pool, filters).await {
        Ok(list) => list,
        Err(e) => {
            log::error!("Failed to list secrets: {}", e);
            return Json(Vec::<SecretOutput>::new()).into_response();
        }
    };

    let secrets = filter_by_namespace(&auth.source, secrets, |s| s.namespace.as_str());

    let output: Vec<SecretOutput> = secrets
        .into_iter()
        .map(|s| SecretOutput {
            id: s.id,
            created_at: s.created_at,
            updated_at: s.updated_at,
            namespace: s.namespace,
            name: s.name,
        })
        .collect();

    Json(output).into_response()
}
