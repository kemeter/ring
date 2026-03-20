use axum::extract::State;
use axum::{Json, response::IntoResponse};

use crate::api::dto::namespace::NamespaceOutput;
use crate::api::server::Db;
use crate::models::namespace as NamespaceModel;
use crate::models::users::User;

pub(crate) async fn list(State(pool): State<Db>, _user: User) -> impl IntoResponse {
    let mut namespaces: Vec<NamespaceOutput> = Vec::new();

    let list_namespaces = match NamespaceModel::find_all(&pool).await {
        Ok(list) => list,
        Err(e) => {
            log::error!("Failed to list namespaces: {}", e);
            return Json(namespaces);
        }
    };

    for namespace in list_namespaces.into_iter() {
        let output = NamespaceOutput::from_to_model(namespace);
        namespaces.push(output);
    }

    Json(namespaces)
}

#[cfg(test)]
mod tests {
    use crate::api::dto::namespace::NamespaceOutput;
    use crate::api::server::tests::login;
    use crate::api::server::tests::new_test_app;
    use axum::http::StatusCode;
    use axum_test::TestServer;

    #[tokio::test]
    async fn list_all_namespaces() {
        let app = new_test_app().await;
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();

        // Create some namespaces first
        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"name": "production"}))
            .await;

        server
            .post("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({"name": "staging"}))
            .await;

        let response = server
            .get("/namespaces")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);

        let namespaces = response.json::<Vec<NamespaceOutput>>();
        assert_eq!(2, namespaces.len());
    }
}
