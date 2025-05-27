use axum::Json;
use serde_json::Value;
use sysinfo::{System, LoadAvg};
use std::env::consts;
use axum::response::IntoResponse;

pub(crate) async fn get() -> impl IntoResponse {
    Json(get_node_info()).into_response()
}

fn get_node_info() -> Value {
    let mut sys = System::new_all();
    sys.refresh_all();

    let load: LoadAvg = System::load_average();

    let memory_total_gib = sys.total_memory()    as f64 / 1024.0 / 1024.0 / 1024.0;   // 3 divisions
    let memory_available_gib = sys.available_memory() as f64 / 1024.0 / 1024.0 / 1024.0;

    serde_json::json!({
        "hostname": System::host_name().unwrap_or_default(),
        "os": consts::OS,
        "arch": consts::ARCH, 
        "uptime": format!("{}s", System::uptime()),
        "cpu_count": sys.cpus().len() as i64,
        "memory_total": memory_total_gib,
        "memory_available": memory_available_gib,
        "load_average": [load.one, load.five, load.fifteen]
    })
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use axum::http::StatusCode;
    use crate::api::server::tests::new_test_app;
    use crate::api::server::tests::login;

    #[tokio::test]
    async fn get() {
        let app = new_test_app();
        let token = login(app.clone(), "admin", "changeme").await;
        let server = TestServer::new(app).unwrap();
        let response = server
            .get("/node/get")
            .add_header("Authorization", format!("Bearer {}", token))
            .await;

        assert_eq!(response.status_code(), StatusCode::OK);
    }
}
