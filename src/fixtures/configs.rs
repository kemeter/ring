use rusqlite::Connection;

pub fn load(connection: &mut Connection) {
    let now = chrono::Utc::now().to_rfc3339();
    
    // Config in kemeter namespace
    connection.execute(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)",
        [
            "cde7806a-21af-473b-968b-08addc7bf0ba",
            &now,
            "kemeter",
            "nginx.conf",
            r#"{"nginx.conf":"server { listen 80; server_name localhost; location / { root /usr/share/nginx/html; index index.html index.htm; } }"}"#,
            "{}"
        ]
    ).unwrap();
    
    // Config in production namespace - app.properties
    connection.execute(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)",
        [
            "bdf9807b-32bf-584c-979c-19bedc8cf1cb",
            &now,
            "production",
            "app.properties", 
            r#"{"app.properties":"database.url=prod.db.com\ndatabase.timeout=30"}"#,
            "{}"
        ]
    ).unwrap();
    
    // Config in production namespace - redis.conf
    connection.execute(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)",
        [
            "ace8906c-43cf-695d-a80d-20cfed9dg2dc",
            &now,
            "production",
            "redis.conf",
            r#"{"redis.conf":"maxmemory 256mb\nmaxmemory-policy allkeys-lru"}"#,
            "{}"
        ]
    ).unwrap();
    
    // Config in staging namespace  
    connection.execute(
        "INSERT INTO config (id, created_at, namespace, name, data, labels) VALUES (?, ?, ?, ?, ?, ?)",
        [
            "def7805d-54df-706e-b91e-31dged0eh3ed",
            &now,
            "staging",
            "app.properties",
            r#"{"app.properties":"database.url=staging.db.com\ndatabase.timeout=10"}"#,
            "{}"
        ]
    ).unwrap();
}