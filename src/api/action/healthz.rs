use axum::Json;
use axum::response::IntoResponse;
use http::StatusCode;

#[derive(serde::Serialize)]
struct Healthz {
    state: String,
}

pub(crate) async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(Healthz {
        state: "UP".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use axum_test::{TestResponse, TestServer};
    use http::StatusCode;
    use crate::api::server::tests::new_test_app;


    #[tokio::test]
    async fn test_healthz_ok() {
        let server = TestServer::new(new_test_app().await).unwrap();

        let response: TestResponse = server
            .get(&"/healthz")
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }
}
