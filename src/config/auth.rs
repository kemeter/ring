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

    resolve_context_token(&context_auth, &context_name)
}

/// Pick a context's token out of a parsed auth.json.
///
/// Split out of [`load_auth_config`] so the missing-context branch is testable:
/// the caller does the I/O, this does the decision.
fn resolve_context_token(
    context_auth: &HashMap<String, AuthToken>,
    context_name: &str,
) -> AuthConfig {
    match context_auth.get(context_name) {
        Some(auth_token) => AuthConfig {
            token: auth_token.token.clone(),
        },
        // Same contract as an unreadable or unparseable file: report and hand
        // back an empty token, letting the caller decide what that means. This
        // used to `process::exit(1)`, which made "no credentials" fatal here
        // but merely empty there -- and broke `ring logout`, whose whole point
        // is to be a successful no-op when there is nothing to log out of.
        None => {
            error!(
                "No credentials for context '{}' in auth.json — run `ring login` or set RING_TOKEN",
                context_name
            );
            AuthConfig {
                token: String::new(),
            }
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

    #[test]
    fn known_context_yields_its_token() {
        let mut map = HashMap::new();
        map.insert(
            "default".to_string(),
            AuthToken {
                token: "abc123".to_string(),
            },
        );

        assert_eq!(resolve_context_token(&map, "default").token, "abc123");
    }

    #[test]
    fn unknown_context_yields_an_empty_token_instead_of_exiting() {
        // Regression guard: this branch used to call `process::exit(1)`, which
        // made `ring logout` impossible when auth.json existed without the
        // current context -- logout is meant to be a no-op success there. It
        // also meant this assertion could not be written at all, since it would
        // have killed the test process.
        let mut map = HashMap::new();
        map.insert(
            "default".to_string(),
            AuthToken {
                token: "abc123".to_string(),
            },
        );

        assert!(
            resolve_context_token(&map, "production").token.is_empty(),
            "a missing context must yield an empty token, not terminate"
        );
    }
}
