use sqlx::SqlitePool;

pub async fn load(pool: &SqlitePool) {
    let now = chrono::Utc::now().to_rfc3339();

    // Config in kemeter namespace
    sqlx::query(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)"
    )
        .bind("cde7806a-21af-473b-968b-08addc7bf0ba")
        .bind(&now)
        .bind("kemeter")
        .bind("nginx.conf")
        .bind(r#"{"nginx.conf":"server { listen 80; server_name localhost; location / { root /usr/share/nginx/html; index index.html index.htm; } }"}"#)
        .bind("{}")
        .execute(pool)
        .await
        .unwrap();

    // Config in production namespace - app.properties
    sqlx::query(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)"
    )
        .bind("bdf9807b-32bf-584c-979c-19bedc8cf1cb")
        .bind(&now)
        .bind("production")
        .bind("app.properties")
        .bind(r#"{"app.properties":"database.url=prod.db.com\ndatabase.timeout=30"}"#)
        .bind("{}")
        .execute(pool)
        .await
        .unwrap();

    // Config in production namespace - redis.conf
    sqlx::query(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)"
    )
        .bind("ace8906c-43cf-695d-a80d-20cfed9dg2dc")
        .bind(&now)
        .bind("production")
        .bind("redis.conf")
        .bind(r#"{"redis.conf":"maxmemory 256mb\nmaxmemory-policy allkeys-lru"}"#)
        .bind("{}")
        .execute(pool)
        .await
        .unwrap();

    // Config in staging namespace
    sqlx::query(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)"
    )
        .bind("def7805d-54df-706e-b91e-31dged0eh3ed")
        .bind(&now)
        .bind("staging")
        .bind("app.properties")
        .bind(r#"{"app.properties":"database.url=staging.db.com\ndatabase.timeout=10"}"#)
        .bind("{}")
        .execute(pool)
        .await
        .unwrap();
}
