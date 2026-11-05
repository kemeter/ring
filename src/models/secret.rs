use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rand::TryRng;
use rand::rngs::SysRng;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::env;

const NONCE_SIZE: usize = 12;

/// Read and validate `RING_SECRET_KEY` from the environment. Returns the
/// decoded 32-byte key on success, an explanatory message otherwise.
///
/// Used both at server startup (so missing/invalid keys produce a clear
/// fatal error before any request is served) and from `ring doctor` to
/// surface configuration drift before users hit it via a panic.
pub(crate) fn try_load_encryption_key() -> Result<[u8; 32], String> {
    parse_encryption_key(env::var("RING_SECRET_KEY").ok().as_deref())
}

/// Decode the value of `RING_SECRET_KEY`, `None` when it is not set.
fn parse_encryption_key(value: Option<&str>) -> Result<[u8; 32], String> {
    let key_str = value.ok_or_else(|| {
        "RING_SECRET_KEY environment variable is not set. \
         Generate one with: openssl rand -base64 32"
            .to_string()
    })?;

    let key_bytes = BASE64
        .decode(key_str)
        .map_err(|e| format!("RING_SECRET_KEY is not valid base64: {}", e))?;

    if key_bytes.len() != 32 {
        return Err(format!(
            "RING_SECRET_KEY must decode to exactly 32 bytes (256 bits), got {}",
            key_bytes.len()
        ));
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);
    Ok(key)
}

fn get_encryption_key() -> [u8; 32] {
    // The server validates the key once at startup (see `commands::server`)
    // and aborts before listening if it's missing or malformed. Anything
    // that reaches this function therefore has a working key — the
    // remaining unwrap is on a path the operator cannot break at runtime.
    try_load_encryption_key().expect("RING_SECRET_KEY validation passed at startup")
}

pub(crate) fn encrypt_value(plaintext: &str) -> Vec<u8> {
    let key = get_encryption_key();
    let cipher = Aes256Gcm::new_from_slice(&key).expect("Invalid key length");

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    SysRng
        .try_fill_bytes(&mut nonce_bytes)
        .expect("OS random generator failed");
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
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

    let nonce =
        Nonce::try_from(&encrypted[..NONCE_SIZE]).map_err(|e| format!("Invalid nonce: {}", e))?;
    let ciphertext = &encrypted[NONCE_SIZE..];

    let plaintext = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|e| format!("Decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| format!("Invalid UTF-8: {}", e))
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

pub(crate) async fn find_all(
    pool: &SqlitePool,
    filters: HashMap<String, Vec<String>>,
) -> Result<Vec<Secret>, sqlx::Error> {
    let (query, values) = crate::models::query::build_filtered_query(
        "SELECT id, created_at, updated_at, namespace, name, value FROM secret",
        &filters,
        ALLOWED_FILTER_COLUMNS,
    );

    let mut q = sqlx::query_as::<_, SecretRow>(sqlx::AssertSqlSafe(query));
    for val in &values {
        q = q.bind(val);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows.into_iter().map(Secret::from).collect())
}

pub(crate) async fn find(pool: &SqlitePool, id: &str) -> Result<Option<Secret>, sqlx::Error> {
    let row = sqlx::query_as::<_, SecretRow>(
        "SELECT id, created_at, updated_at, namespace, name, value FROM secret WHERE id = ?",
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

/// Key every test encrypts with. Not all zeros, so code that ignored the
/// configured key and fell back to a zeroed one would fail the tests.
#[cfg(test)]
pub(crate) const TEST_KEY: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// Set `RING_SECRET_KEY` to `TEST_KEY`. Every test sets the same value, so
/// tests running in parallel never see another key.
#[cfg(test)]
pub(crate) fn set_test_key() {
    unsafe { env::set_var("RING_SECRET_KEY", BASE64.encode(TEST_KEY)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        set_test_key();

        let plaintext = "my-secret-value";
        let encrypted = encrypt_value(plaintext);
        let decrypted = decrypt_value(&encrypted).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    /// A value produced outside Ring (Node's AES-256-GCM under `TEST_KEY`), so
    /// a dependency upgrade can't change the format and still pass a
    /// roundtrip, and a key other than the configured one fails to read it.
    #[test]
    fn test_decrypts_a_value_stored_earlier() {
        set_test_key();

        let stored = hex::decode(
            "000102030405060708090a0b3476b969a081e279e827f8f9d4c90c05e6f6f24497093e185d0a8adab019eabbb830b9d9c1e150fe76",
        )
        .unwrap();

        assert_eq!(decrypt_value(&stored).unwrap(), "stored before the upgrade");
    }

    #[test]
    fn test_data_shorter_than_a_nonce_is_rejected() {
        set_test_key();

        assert!(decrypt_value(&[0u8; NONCE_SIZE - 1]).is_err());
        assert!(decrypt_value(&[]).is_err());
    }

    #[test]
    fn test_a_valid_key_is_decoded() {
        assert_eq!(
            parse_encryption_key(Some(&BASE64.encode(TEST_KEY))),
            Ok(TEST_KEY)
        );
    }

    #[test]
    fn test_a_missing_key_is_refused() {
        let err = parse_encryption_key(None).unwrap_err();
        assert!(err.contains("not set"), "{err}");
    }

    #[test]
    fn test_a_key_that_is_not_base64_is_refused() {
        let err = parse_encryption_key(Some("not base64!")).unwrap_err();
        assert!(err.contains("not valid base64"), "{err}");
    }

    #[test]
    fn test_a_key_of_the_wrong_length_is_refused() {
        let err = parse_encryption_key(Some(&BASE64.encode([1u8; 16]))).unwrap_err();
        assert!(err.contains("got 16"), "{err}");
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
