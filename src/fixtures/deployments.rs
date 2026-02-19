use sqlx::SqlitePool;

pub async fn load(pool: &SqlitePool) {
    // Pending deployment in default namespace
    sqlx::query(
        "INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("658c0199-85a2-49da-86d6-1ecd2e427118")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("pending")
        .bind("default")
        .bind("nginx")
        .bind("nginx")
        .bind("1")
        .bind("docker")
        .bind("worker")
        .bind("[]")
        .bind("[]")
        .bind("[]")
        .execute(pool)
        .await
        .unwrap();

    // Running deployment in default namespace
    sqlx::query(
        "INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("759d1280-95a3-40da-86d6-2fde3f538229")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("running")
        .bind("default")
        .bind("php:8.3")
        .bind("php")
        .bind("1")
        .bind("docker")
        .bind("worker")
        .bind("[]")
        .bind("[]")
        .bind("[]")
        .execute(pool)
        .await
        .unwrap();

    // Pending deployment in kemeter namespace
    sqlx::query(
        "INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
        .bind("860e1381-a6b4-51eb-97e7-3gf1416fg340")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("pending")
        .bind("kemeter")
        .bind("php:8.3")
        .bind("php")
        .bind("1")
        .bind("docker")
        .bind("worker")
        .bind("[]")
        .bind("[]")
        .bind("[]")
        .execute(pool)
        .await
        .unwrap();
}
