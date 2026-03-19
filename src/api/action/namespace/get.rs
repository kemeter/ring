use axum::extract::State;
use axum::{Json, extract::Path, response::IntoResponse};

use crate::api::dto::namespace::NamespaceOutput;
use crate::api::server::Db;
use crate::models::namespace;
use crate::models::users::User;
use axum::http::StatusCode;

pub(crate) async fn get(
    Path(id): Path<String>,
    _user: User,
    State(pool): State<Db>,
) -> impl IntoResponse {
    match namespace::find(&pool, id.clone()).await {
        Ok(Some(ns)) => {
            let output = NamespaceOutput::from_to_model(ns);
            Json(output).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::dto::namespace::NamespaceOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn not_found() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/namespaces/nonexistent")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_namespace() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Create a namespace first
        let create_response = server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"name": "test-ns"}))
            .await;

        let created = create_response.json::<NamespaceOutput>();

        let response = server
            .get(&format!("/namespaces/{}", created.id))
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
        let ns = response.json::<NamespaceOutput>();
        assert_eq!(ns.name, "test-ns");
    }
}
