use rusqlite::Connection;

pub fn load(connection: &mut Connection) {
    // Admin user for tests - using the same ID as the original fixture
    connection.execute(
        "INSERT INTO user (id, created_at, status, username, password, token) VALUES (?, ?, ?, ?, ?, ?)",
        [
            "5b5c370a-cdbf-4fa4-826e-1eea4d8f7d47",
            &chrono::Utc::now().to_rfc3339(),
            "active",
            "admin",
            "$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$NtAhPV3e8INMg6E1LnAE5wIHd/YszYoEyZeF0+1zT8E", // changeme
            "johndoetoken" // Keep the same token as before to not break existing tests
        ]
    ).unwrap();
    
    // Second user for tests that need multiple users
    connection.execute(
        "INSERT INTO user (id, created_at, status, username, password, token) VALUES (?, ?, ?, ?, ?, ?)",
        [
            "6c6d481b-debf-5gb5-937f-2ffa5e9f8e58",
            &chrono::Utc::now().to_rfc3339(),
            "active", 
            "john.doe",
            "$argon2id$v=19$m=65536,t=2,p=4$Y2hhbmdlbWU$NtAhPV3e8INMg6E1LnAE5wIHd/YszYoEyZeF0+1zT8E", // changeme
            "john_token"
        ]
    ).unwrap();
}