use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::api::auth::{Auth, AuthSource};
use crate::api::dto::namespace::NamespaceOutput;
use crate::api::server::Db;
use crate::api::validation::{Violation, ViolationList, problem_response};
use crate::config::config::Config;
use crate::models::audit_log;
use crate::models::namespace;
use crate::models::security_level::SecurityLevel;

/// `PATCH /namespaces/{name}`: set the namespace's security level, or reset it
/// to the server's default with `null`.
///
/// The route requires `admin` (see `scope_for_route`). A level decides whether
/// a namespace's deployments may reach the host, and with it every other
/// namespace, so a token limited to some namespaces is refused as well: it
/// could otherwise open the host to itself.
pub(crate) async fn update(
    Path(name): Path<String>,
    State(pool): State<Db>,
    State(configuration): State<Config>,
    auth: Auth,
    Json(input): Json<serde_json::Map<String, serde_json::Value>>,
) -> Response {
    if let AuthSource::Token { namespaces, .. } = &auth.source
        && !namespaces.is_empty()
    {
        return problem_response(
            StatusCode::FORBIDDEN,
            "Forbidden",
            "a token limited to some namespaces cannot change a security level",
        );
    }

    let level = match input.get("security_level") {
        None => {
            return violation(
                "security_level is required",
                "namespace.security_level.required",
            );
        }
        Some(serde_json::Value::Null) => None,
        Some(value) => match value.as_str().map(str::parse::<SecurityLevel>) {
            Some(Ok(level)) => Some(level),
            _ => {
                return violation(
                    "must be privileged, baseline, restricted or null",
                    "namespace.security_level.unknown",
                );
            }
        },
    };

    let mut found = match namespace::find_by_name(&pool, &name).await {
        Ok(Some(found)) => found,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("Failed to look up namespace '{}': {}", name, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if let Err(e) = namespace::set_security_level(&pool, &name, level).await {
        error!("Failed to set the security level of '{}': {}", name, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    found.security_level = level;

    let _ = audit_log::record(
        &pool,
        Some(&auth.user.id),
        "update",
        "namespace",
        &name,
        Some(&name),
    )
    .await;

    Json(NamespaceOutput::from_to_model(
        found,
        configuration.server.admission.default_level,
    ))
    .into_response()
}

fn violation(message: &str, code: &str) -> Response {
    let mut violations = ViolationList::new();
    violations.push(Violation::new("security_level", message, code));
    violations.into_response()
}

#[cfg(test)]
mod tests {
    use crate::api::testing::TestApp;
    use crate::models::audit_log;
    use crate::models::users::Role;
    use axum::http::StatusCode;
    use serde_json::json;

    async fn app_with_namespace() -> TestApp {
        let app = TestApp::admin().await;
        app.namespace("tenant-a").await;
        app
    }

    #[tokio::test]
    async fn an_admin_sets_and_resets_the_level() {
        let app = app_with_namespace().await;

        let response = app
            .patch("/namespaces/tenant-a")
            .json(&json!({ "security_level": "privileged" }))
            .await;
        response.assert_status_ok();
        assert_eq!(
            response.json::<serde_json::Value>()["security_level"],
            "privileged"
        );

        let response = app
            .patch("/namespaces/tenant-a")
            .json(&json!({ "security_level": null }))
            .await;
        response.assert_status_ok();
        assert_eq!(
            response.json::<serde_json::Value>()["security_level"],
            "baseline",
            "back to the server's default"
        );

        let entries = audit_log::find_by_namespace(&app.pool, "tenant-a", None)
            .await
            .unwrap();
        assert!(entries.iter().any(|e| e.action == "update"));
    }

    #[tokio::test]
    async fn the_new_level_applies_to_the_next_deployment() {
        let app = app_with_namespace().await;
        let docker_socket = json!({
            "runtime": "docker",
            "name": "agent",
            "namespace": "tenant-a",
            "image": "alpine:3",
            "volumes": [{
                "type": "bind",
                "source": "/var/run/docker.sock",
                "destination": "/var/run/docker.sock",
                "driver": "local",
                "permission": "ro"
            }]
        });

        app.post("/deployments")
            .json(&docker_socket)
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

        app.patch("/namespaces/tenant-a")
            .json(&json!({ "security_level": "privileged" }))
            .await
            .assert_status_ok();
        app.post("/deployments")
            .json(&docker_socket)
            .await
            .assert_status(StatusCode::CREATED);
    }

    #[tokio::test]
    async fn an_operator_cannot_change_the_level() {
        let mut app = app_with_namespace().await;
        let operator = app.user(Role::Operator).await;
        let session = app.session(&operator, Role::Operator).await;
        app.authenticate(&session);

        app.patch("/namespaces/tenant-a")
            .json(&json!({ "security_level": "privileged" }))
            .await
            .assert_status_forbidden();
    }

    #[tokio::test]
    async fn an_admin_token_limited_to_the_namespace_cannot_open_it_to_the_host() {
        let mut app = app_with_namespace().await;
        app.act_as_pat(&["admin"], &["tenant-a"]).await;

        app.patch("/namespaces/tenant-a")
            .json(&json!({ "security_level": "privileged" }))
            .await
            .assert_status_forbidden();
        let level = crate::models::namespace::find_by_name(&app.pool, "tenant-a")
            .await
            .unwrap()
            .unwrap()
            .security_level;
        assert_eq!(level, None);
    }

    #[tokio::test]
    async fn an_unknown_or_missing_level_is_refused() {
        let app = app_with_namespace().await;
        for (body, code) in [
            (
                json!({ "security_level": "root" }),
                "namespace.security_level.unknown",
            ),
            (
                json!({ "security_level": 1 }),
                "namespace.security_level.unknown",
            ),
            (json!({}), "namespace.security_level.required"),
        ] {
            let response = app.patch("/namespaces/tenant-a").json(&body).await;
            response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(
                response.json::<serde_json::Value>()["violations"][0]["code"],
                code,
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_namespace_is_not_found() {
        let app = TestApp::admin().await;
        app.patch("/namespaces/nowhere")
            .json(&json!({ "security_level": "baseline" }))
            .await
            .assert_status_not_found();
    }
}
