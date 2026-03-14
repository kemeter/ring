use std::collections::HashMap;
use std::env;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use aes_gcm::aead::rand_core::RngCore;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

const NONCE_SIZE: usize = 12;

fn get_encryption_key() -> [u8; 32] {
    let key_str = env::var("RING_SECRET_KEY")
        .expect("RING_SECRET_KEY environment variable must be set (32 bytes, base64 encoded)");

    let key_bytes = BASE64.decode(&key_str)
        .expect("RING_SECRET_KEY must be valid base64");

    if key_bytes.len() != 32 {
        panic!("RING_SECRET_KEY must be exactly 32 bytes (256 bits)");
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);
    key
}

pub(crate) fn encrypt_value(plaintext: &str) -> Vec<u8> {
    let key = get_encryption_key();
    let cipher = Aes256Gcm::new_from_slice(&key).expect("Invalid key length");

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .expect("Encryption failed");

    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    result
}

pub(crate) fn decrypt_value(encrypted: &[u8]) -> Result<String, String> {
    if encrypted.len() < NONCE_SIZE {
        return Err("Invalid encrypted data: too short".to_string());
    }

    let key = get_encryption_key();
    let cipher = Aes256Gcm::new_from_slice(&key).expect("Invalid key length");

    let nonce = Nonce::from_slice(&encrypted[..NONCE_SIZE]);
    let ciphertext = &encrypted[NONCE_SIZE..];

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("Decryption failed: {}", e))?;

    String::from_utf8(plaintext)
        .map_err(|e| format!("Invalid UTF-8: {}", e))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Secret {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) namespace: String,
    pub(crate) name: String,
    #[serde(skip_serializing)]
    pub(crate) value: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct SecretRow {
    id: String,
    created_at: String,
    updated_at: Option<String>,
    namespace: String,
    name: String,
    value: Vec<u8>,
}

impl From<SecretRow> for Secret {
    fn from(row: SecretRow) -> Self {
        Secret {
            id: row.id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            namespace: row.namespace,
            name: row.name,
            value: row.value,
        }
    }
}

impl Secret {
    pub(crate) fn get_decrypted_value(&self) -> Result<String, String> {
        decrypt_value(&self.value)
    }
}

const ALLOWED_FILTER_COLUMNS: &[&str] = &["namespace"];

pub(crate) async fn find_all(pool: &SqlitePool, filters: HashMap<String, Vec<String>>) -> Vec<Secret> {
    let mut query = String::from("SELECT id, created_at, updated_at, namespace, name, value FROM secret");
    let mut all_values: Vec<String> = Vec::new();

    if !filters.is_empty() {
        let conditions: Vec<String> = filters
            .iter()
            .filter(|(k, v)| !v.is_empty() && ALLOWED_FILTER_COLUMNS.contains(&k.as_str()))
            .map(|(column, values)| {
                let placeholders = values.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                all_values.extend(values.clone());
                format!("{} IN({})", column, placeholders)
            })
            .collect();

        if !conditions.is_empty() {
            query += &format!(" WHERE {}", conditions.join(" AND "));
        }
    }

    let mut q = sqlx::query_as::<_, SecretRow>(&query);
    for val in &all_values {
        q = q.bind(val);
    }

    match q.fetch_all(pool).await {
        Ok(rows) => rows.into_iter().map(Secret::from).collect(),
        Err(e) => {
            log::error!("Failed to execute secret query: {}", e);
            vec![]
        }
    }
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Secret>, sqlx::Error> {
    let row = sqlx::query_as::<_, SecretRow>(
        "SELECT id, created_at, updated_at, namespace, name, value FROM secret WHERE id = ?"
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(Secret::from))
}

pub(crate) async fn find_by_namespace_name(
    pool: &SqlitePool,
    namespace: &str,
    name: &str,
) -> Result<Option<Secret>, sqlx::Error> {
    let row = sqlx::query_as::<_, SecretRow>(
        "SELECT id, created_at, updated_at, namespace, name, value FROM secret WHERE namespace = ? AND name = ?"
    )
    .bind(namespace)
    .bind(name)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(Secret::from))
}

pub(crate) async fn create(pool: &SqlitePool, secret: &Secret) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO secret (id, created_at, updated_at, namespace, name, value) VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(&secret.id)
    .bind(&secret.created_at)
    .bind(&secret.updated_at)
    .bind(&secret.namespace)
    .bind(&secret.name)
    .bind(&secret.value)
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn delete(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    let result = sqlx::query("DELETE FROM secret WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(sqlx::Error::RowNotFound);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_test_key() {
        let key = [0u8; 32];
        let key_b64 = BASE64.encode(key);
        unsafe { env::set_var("RING_SECRET_KEY", key_b64) };
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        set_test_key();

        let plaintext = "my-secret-value";
        let encrypted = encrypt_value(plaintext);
        let decrypted = decrypt_value(&encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypted_value_contains_nonce() {
        set_test_key();

        let plaintext = "test";
        let encrypted = encrypt_value(plaintext);

        assert!(encrypted.len() > NONCE_SIZE);
    }

    #[test]
    fn test_different_encryptions_produce_different_ciphertexts() {
        set_test_key();

        let plaintext = "same-value";
        let encrypted1 = encrypt_value(plaintext);
        let encrypted2 = encrypt_value(plaintext);

        assert_ne!(encrypted1, encrypted2);
    }
}
