use crate::config::config::get_config_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthConfig {
    pub(crate) token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct AuthToken {
    token: String,
}

fn auth_token_from_env<F>(get_var: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    get_var("RING_TOKEN").filter(|t| !t.is_empty())
}

pub(crate) fn load_auth_config(context_name: String) -> AuthConfig {
    // RING_TOKEN takes precedence over auth.json. Useful for CI and
    // stateless environments where running `ring login` is impractical.
    if let Some(token) = auth_token_from_env(|k| env::var(k).ok()) {
        return AuthConfig { token };
    }

    let home_dir = get_config_dir();
    let file = format!("{}/auth.json", home_dir);
    let auth_file_content = match fs::read_to_string(file) {
        Ok(content) => content,
        Err(e) => {
            error!("Failed to read auth file: {}", e);
            return AuthConfig {
                token: String::new(),
            };
        }
    };

    let context_auth: HashMap<String, AuthToken> = match serde_json::from_str(&auth_file_content) {
        Ok(auth) => auth,
        Err(e) => {
            error!("Failed to parse auth file: {}", e);
            return AuthConfig {
                token: String::new(),
            };
        }
    };

    match context_auth.get(&context_name) {
        Some(auth_token) => AuthConfig {
            token: auth_token.token.clone(),
        },
        None => {
            eprintln!(
                "Error: Context '{}' does not exist in a configuration file",
                context_name
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_token_from_env_returns_some_when_set() {
        let token = auth_token_from_env(|k| {
            if k == "RING_TOKEN" {
                Some("abc123".to_string())
            } else {
                None
            }
        });
        assert_eq!(token.as_deref(), Some("abc123"));
    }

    #[test]
    fn ring_token_from_env_is_none_when_unset() {
        let token = auth_token_from_env(|_| None);
        assert!(token.is_none());
    }

    #[test]
    fn ring_token_from_env_is_none_when_empty() {
        let token = auth_token_from_env(|k| {
            if k == "RING_TOKEN" {
                Some(String::new())
            } else {
                None
            }
        });
        assert!(token.is_none());
    }
}
