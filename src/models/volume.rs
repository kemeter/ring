use crate::api::dto::deployment::DeploymentVolume;
use crate::models::config::Config;
use crate::models::secret::Secret;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedMount {
    Bind {
        source: String,
        destination: String,
        read_only: bool,
    },
    Named {
        name: String,
        destination: String,
        read_only: bool,
        driver: String,
    },
    Content {
        content: String,
        destination: String,
    },
}

pub fn resolve_volumes(
    volumes_json: &str,
    configs: &HashMap<String, Config>,
    secrets: &HashMap<String, Secret>,
) -> Result<Vec<ResolvedMount>, String> {
    let volumes: Vec<DeploymentVolume> = serde_json::from_str(volumes_json)
        .map_err(|e| format!("Failed to parse volumes: {}", e))?;

    let mut resolved = Vec::new();

    for volume in volumes {
        let mount = match volume.r#type.as_str() {
            "bind" => {
                let source = volume.source.ok_or("Bind volume requires a source")?;
                ResolvedMount::Bind {
                    source,
                    destination: volume.destination,
                    read_only: volume.permission == "ro",
                }
            }
            "volume" => {
                let name = volume.source.ok_or("Named volume requires a source")?;
                ResolvedMount::Named {
                    name,
                    destination: volume.destination,
                    read_only: volume.permission == "ro",
                    driver: volume.driver,
                }
            }
            "config" => {
                let config_name = volume
                    .source
                    .as_ref()
                    .ok_or("Config volume requires a source")?;

                let config = configs
                    .get(config_name)
                    .ok_or(format!("Config '{}' not found", config_name))?;

                let config_data: HashMap<String, String> = serde_json::from_str(&config.data)
                    .map_err(|e| format!("Failed to parse config data: {}", e))?;

                let key = volume
                    .key
                    .as_ref()
                    .ok_or("Missing 'key' field for config volume")?;

                let content = config_data
                    .get(key)
                    .ok_or(format!(
                        "Key '{}' not found in config '{}'",
                        key, config_name
                    ))?
                    .clone();

                ResolvedMount::Content {
                    content,
                    destination: volume.destination,
                }
            }
            "secret" => {
                // A secret is a single opaque string — there is no `key:`,
                // its whole decrypted value becomes the file contents. The
                // mount is always read-only: containers should never write
                // back into a secret.
                let secret_name = volume
                    .source
                    .as_ref()
                    .ok_or("Secret volume requires a source")?;

                let secret = secrets
                    .get(secret_name)
                    .ok_or(format!("Secret '{}' not found", secret_name))?;

                let content = secret
                    .get_decrypted_value()
                    .map_err(|e| format!("Failed to decrypt secret '{}': {}", secret_name, e))?;

                ResolvedMount::Content {
                    content,
                    destination: volume.destination,
                }
            }
            other => return Err(format!("Unknown volume type '{}'", other)),
        };
        resolved.push(mount);
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::secret::encrypt_value;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use std::env;

    fn make_config(name: &str, data: &str) -> Config {
        Config {
            id: "test-id".to_string(),
            created_at: "2024-01-01".to_string(),
            updated_at: None,
            namespace: "default".to_string(),
            name: name.to_string(),
            data: data.to_string(),
            labels: "{}".to_string(),
        }
    }

    fn set_test_key() {
        let key = [0u8; 32];
        let key_b64 = BASE64.encode(key);
        unsafe { env::set_var("RING_SECRET_KEY", key_b64) };
    }

    fn make_secret(name: &str, plaintext: &str) -> Secret {
        set_test_key();
        Secret {
            id: "test-secret-id".to_string(),
            created_at: "2024-01-01".to_string(),
            updated_at: None,
            namespace: "default".to_string(),
            name: name.to_string(),
            value: encrypt_value(plaintext),
        }
    }

    #[test]
    fn resolve_bind_volume() {
        let json = r#"[{"type":"bind","source":"/host/path","destination":"/container/path","driver":"local","permission":"ro"}]"#;
        let result = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap();

        assert_eq!(result.len(), 1);
        match &result[0] {
            ResolvedMount::Bind {
                source,
                destination,
                read_only,
            } => {
                assert_eq!(source, "/host/path");
                assert_eq!(destination, "/container/path");
                assert!(read_only);
            }
            _ => panic!("Expected Bind mount"),
        }
    }

    #[test]
    fn resolve_bind_volume_rw() {
        let json = r#"[{"type":"bind","source":"/data","destination":"/app/data","driver":"local","permission":"rw"}]"#;
        let result = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap();

        match &result[0] {
            ResolvedMount::Bind { read_only, .. } => assert!(!read_only),
            _ => panic!("Expected Bind mount"),
        }
    }

    #[test]
    fn resolve_named_volume() {
        let json = r#"[{"type":"volume","source":"my-data","destination":"/data","driver":"local","permission":"rw"}]"#;
        let result = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap();

        assert_eq!(result.len(), 1);
        match &result[0] {
            ResolvedMount::Named {
                name,
                destination,
                read_only,
                driver,
            } => {
                assert_eq!(name, "my-data");
                assert_eq!(destination, "/data");
                assert!(!read_only);
                assert_eq!(driver, "local");
            }
            _ => panic!("Expected Named mount"),
        }
    }

    #[test]
    fn resolve_named_volume_with_nfs_driver() {
        let json = r#"[{"type":"volume","source":"shared","destination":"/mnt","driver":"nfs","permission":"ro"}]"#;
        let result = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap();

        match &result[0] {
            ResolvedMount::Named {
                driver, read_only, ..
            } => {
                assert_eq!(driver, "nfs");
                assert!(read_only);
            }
            _ => panic!("Expected Named mount"),
        }
    }

    #[test]
    fn resolve_config_volume() {
        let mut configs = HashMap::new();
        configs.insert(
            "nginx-config".to_string(),
            make_config("nginx-config", r#"{"nginx.conf":"server { listen 80; }"}"#),
        );

        let json = r#"[{"type":"config","source":"nginx-config","key":"nginx.conf","destination":"/etc/nginx/nginx.conf","driver":"local","permission":"ro"}]"#;
        let result = resolve_volumes(json, &configs, &HashMap::new()).unwrap();

        assert_eq!(result.len(), 1);
        match &result[0] {
            ResolvedMount::Content {
                content,
                destination,
            } => {
                assert_eq!(content, "server { listen 80; }");
                assert_eq!(destination, "/etc/nginx/nginx.conf");
            }
            _ => panic!("Expected Content mount"),
        }
    }

    #[test]
    fn resolve_config_not_found() {
        let json = r#"[{"type":"config","source":"missing","key":"file","destination":"/etc/conf","driver":"local","permission":"ro"}]"#;
        let err = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(err.contains("Config 'missing' not found"));
    }

    #[test]
    fn resolve_config_key_not_found() {
        let mut configs = HashMap::new();
        configs.insert(
            "my-config".to_string(),
            make_config("my-config", r#"{"other-key":"value"}"#),
        );

        let json = r#"[{"type":"config","source":"my-config","key":"missing-key","destination":"/etc/conf","driver":"local","permission":"ro"}]"#;
        let err = resolve_volumes(json, &configs, &HashMap::new()).unwrap_err();
        assert!(err.contains("Key 'missing-key' not found"));
    }

    #[test]
    fn resolve_unknown_volume_type() {
        let json = r#"[{"type":"tmpfs","source":"x","destination":"/tmp","driver":"local","permission":"rw"}]"#;
        let err = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(err.contains("Unknown volume type 'tmpfs'"));
    }

    #[test]
    fn resolve_bind_missing_source() {
        let json = r#"[{"type":"bind","destination":"/data","driver":"local","permission":"rw"}]"#;
        let err = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(err.contains("Bind volume requires a source"));
    }

    #[test]
    fn resolve_multiple_volumes() {
        let mut configs = HashMap::new();
        configs.insert(
            "app-config".to_string(),
            make_config("app-config", r#"{"app.toml":"[server]\nport = 8080"}"#),
        );

        let json = r#"[
            {"type":"bind","source":"/var/log","destination":"/logs","driver":"local","permission":"rw"},
            {"type":"volume","source":"db-data","destination":"/data","driver":"local","permission":"rw"},
            {"type":"config","source":"app-config","key":"app.toml","destination":"/etc/app.toml","driver":"local","permission":"ro"}
        ]"#;
        let result = resolve_volumes(json, &configs, &HashMap::new()).unwrap();

        assert_eq!(result.len(), 3);
        assert!(matches!(result[0], ResolvedMount::Bind { .. }));
        assert!(matches!(result[1], ResolvedMount::Named { .. }));
        assert!(matches!(result[2], ResolvedMount::Content { .. }));
    }

    #[test]
    fn resolve_empty_volumes() {
        let result = resolve_volumes("[]", &HashMap::new(), &HashMap::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn resolve_secret_volume() {
        let mut secrets = HashMap::new();
        secrets.insert(
            "api-token".to_string(),
            make_secret("api-token", "s3cr3t-bearer-token"),
        );

        let json = r#"[{"type":"secret","source":"api-token","destination":"/run/secrets/api-token","driver":"local","permission":"ro"}]"#;
        let result = resolve_volumes(json, &HashMap::new(), &secrets).unwrap();

        assert_eq!(result.len(), 1);
        match &result[0] {
            ResolvedMount::Content {
                content,
                destination,
            } => {
                assert_eq!(content, "s3cr3t-bearer-token");
                assert_eq!(destination, "/run/secrets/api-token");
            }
            _ => panic!("Expected Content mount for secret"),
        }
    }

    #[test]
    fn resolve_secret_not_found() {
        let json = r#"[{"type":"secret","source":"missing","destination":"/run/secrets/x","driver":"local","permission":"ro"}]"#;
        let err = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(err.contains("Secret 'missing' not found"));
    }

    #[test]
    fn resolve_secret_missing_source() {
        let json = r#"[{"type":"secret","destination":"/run/secrets/x","driver":"local","permission":"ro"}]"#;
        let err = resolve_volumes(json, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(err.contains("Secret volume requires a source"));
    }

    #[test]
    fn resolve_secret_alongside_config() {
        let mut configs = HashMap::new();
        configs.insert(
            "app-config".to_string(),
            make_config("app-config", r#"{"app.toml":"[server]"}"#),
        );
        let mut secrets = HashMap::new();
        secrets.insert("db-token".to_string(), make_secret("db-token", "deadbeef"));

        let json = r#"[
            {"type":"config","source":"app-config","key":"app.toml","destination":"/etc/app.toml","driver":"local","permission":"ro"},
            {"type":"secret","source":"db-token","destination":"/run/secrets/db-token","driver":"local","permission":"ro"}
        ]"#;
        let result = resolve_volumes(json, &configs, &secrets).unwrap();

        assert_eq!(result.len(), 2);
        match &result[1] {
            ResolvedMount::Content { content, .. } => assert_eq!(content, "deadbeef"),
            _ => panic!("Expected Content mount"),
        }
    }
}
