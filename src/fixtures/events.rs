use sqlx::SqlitePool;

pub async fn load(pool: &SqlitePool) {
    let now = chrono::Utc::now().to_rfc3339();

    // Event for deployment creation
    sqlx::query(
        "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("event-1")
        .bind("658c0199-85a2-49da-86d6-1ecd2e427118") // Links to nginx deployment
        .bind(&now)
        .bind("info")
        .bind("Deployment created successfully")
        .bind("api")
        .bind("DeploymentCreated")
        .execute(pool)
        .await
        .unwrap();

    // Event for deployment error
    sqlx::query(
        "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("event-2")
        .bind("658c0199-85a2-49da-86d6-1ecd2e427118") // Links to nginx deployment
        .bind(&now)
        .bind("error")
        .bind("Failed to pull image nginx:latest")
        .bind("docker")
        .bind("ImagePullError")
        .execute(pool)
        .await
        .unwrap();
}
