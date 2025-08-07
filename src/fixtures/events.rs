use rusqlite::Connection;

pub fn load(connection: &mut Connection) {
    let now = chrono::Utc::now().to_rfc3339();
    
    // Event for deployment creation
    connection.execute(
        "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason) VALUES (?, ?, ?, ?, ?, ?, ?)",
        [
            "event-1",
            "658c0199-85a2-49da-86d6-1ecd2e427118", // Links to nginx deployment
            &now,
            "info",
            "Deployment created successfully",
            "api",
            "DeploymentCreated"
        ]
    ).unwrap();
    
    // Event for deployment error
    connection.execute(
        "INSERT INTO deployment_event (id, deployment_id, timestamp, level, message, component, reason) VALUES (?, ?, ?, ?, ?, ?, ?)",
        [
            "event-2",
            "658c0199-85a2-49da-86d6-1ecd2e427118", // Links to nginx deployment
            &now,
            "error",
            "Failed to pull image nginx:latest",
            "docker",
            "ImagePullError"
        ]
    ).unwrap();
}