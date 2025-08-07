use rusqlite::Connection;

pub fn load(connection: &mut Connection) {
    // Pending deployment in default namespace
    connection.execute(
        "INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            "658c0199-85a2-49da-86d6-1ecd2e427118",
            &chrono::Utc::now().to_rfc3339(),
            "pending",
            "default",
            "nginx",
            "nginx",
            "1",
            "docker",
            "worker",
            "[]",
            "[]",
            "[]"
        ]
    ).unwrap();

    // Running deployment in default namespace
    connection.execute(
        "INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            "759d1280-95a3-40da-86d6-2fde3f538229",
            &chrono::Utc::now().to_rfc3339(),
            "running",
            "default",
            "php:8.3",
            "php",
            "1", 
            "docker",
            "worker",
            "[]",
            "[]",
            "[]"
        ]
    ).unwrap();

    // Pending deployment in kemeter namespace - correcting the original duplicate ID issue
    connection.execute(
        "INSERT INTO deployment (id, created_at, status, namespace, name, image, replicas, runtime, kind, labels, secrets, volumes) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            "860e1381-a6b4-51eb-97e7-3gf1416fg340",
            &chrono::Utc::now().to_rfc3339(),
            "pending",
            "kemeter", 
            "php:8.3",
            "php",
            "1",
            "docker", // Fix: was "kemeter", should be "docker"
            "worker",
            "[]",
            "[]",
            "[]"
        ]
    ).unwrap();
}