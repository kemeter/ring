use crate::api::action::webhook::WebhookView;
use crate::api::auth::Auth;
use crate::api::server::Db;
use crate::api::validation::problem_response;
use crate::models::webhook;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub(crate) async fn list(State(pool): State<Db>, _auth: Auth) -> Response {
    // Scope (`webhooks:read`) is enforced centrally by the auth middleware.
    match webhook::find_all(&pool).await {
        Ok(hooks) => {
            let views: Vec<WebhookView> = hooks.into_iter().map(WebhookView::from).collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(e) => {
            log::error!("Failed to list webhooks: {}", e);
            problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error",
                "failed to list webhooks",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::server::tests::new_test_app_with_pool;
    use crate::models::token;
    use crate::models::webhook;
    use axum_test::TestServer;
    use http::StatusCode;

    const ADMIN_ID: &str = "1c5a5fe9-84e0-4a18-821e-8058232c2c23";

    async fn pat(pool: &sqlx::SqlitePool, scopes: &[&str]) -> String {
        let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        let (clear, _) = token::create(pool, ADMIN_ID, "test", &scopes, &[], None)
            .await
            .expect("create token");
        clear
    }

    #[tokio::test]
    async fn list_response_never_contains_secret() {
        // Invariant: the HMAC secret is stored but must never be serialized back
        // on a read path. Guards both `#[serde(skip_serializing)]` on the model
        // and the secret-free `WebhookView` projection.
        let (pool, app) = new_test_app_with_pool().await;
        let secret = "whsec_super_secret_value_do_not_leak";
        webhook::create(&pool, "https://hooks.example.com/ring", Some(secret), &[])
            .await
            .expect("seed webhook");
        let token = pat(&pool, &["webhooks:read"]).await;
        let server = TestServer::new(app).unwrap();

        let res = server
            .get("/webhooks")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(res.status_code(), StatusCode::OK);
        let body = res.text();
        assert!(
            !body.contains(secret),
            "list response leaked the webhook secret: {body}"
        );
        assert!(
            !body.contains("\"secret\""),
            "list response exposed a secret field: {body}"
        );
    }
}
