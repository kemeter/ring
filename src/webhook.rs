//! Webhook delivery.
//!
//! Turns a queued event into a signed HTTP POST to a subscriber. Called by the
//! event worker, never inline in the scheduler — delivery latency and failures
//! must not touch the reconciliation loop.
//!
//! The body is the event's JSON payload. When the subscriber has a secret, the
//! POST carries an `X-Ring-Signature: sha256=<hex>` header (HMAC-SHA256 of the
//! body), the GitHub/Stripe convention, so the receiver can authenticate it.

use crate::models::webhook::Webhook;
use std::sync::OnceLock;
use std::time::Duration;

/// Shared delivery client, built once. Reusing it keeps the connection pool and
/// TLS config warm across deliveries instead of rebuilding both on every POST.
///
/// Redirects are disabled (`Policy::none()`): a subscriber URL is user-supplied
/// and following a 3xx would let it bounce the server-side request to an
/// internal address (e.g. cloud metadata at 169.254.169.254), defeating the
/// host allowlist enforced at creation. A redirecting subscriber just fails
/// delivery and is retried/dead-lettered like any other non-2xx.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build webhook delivery client")
    })
}

/// HMAC-SHA256 of `body` keyed by `secret`, formatted `sha256=<hex>`.
fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Deliver `body` (the event payload JSON) to `hook`. Returns `Ok(())` on a 2xx,
/// `Err(reason)` otherwise (non-2xx or transport error) so the worker can
/// decide whether to retry or dead-letter. Never panics.
pub(crate) async fn deliver(hook: &Webhook, kind: &str, body: &[u8]) -> Result<(), String> {
    let mut request = client()
        .post(&hook.url)
        .header("content-type", "application/json")
        .header("user-agent", concat!("ring/", env!("CARGO_PKG_VERSION")))
        .header("x-ring-event", kind)
        .timeout(Duration::from_secs(10))
        .body(body.to_vec());

    if let Some(secret) = &hook.secret {
        request = request.header("x-ring-signature", sign(secret, body));
    }

    match request.send().await {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(format!("subscriber returned {}", response.status())),
        Err(e) => Err(format!("request to {} failed: {}", hook.url, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::sign;

    #[test]
    fn sign_matches_known_hmac_vector() {
        // HMAC-SHA256(key="secret", msg="body"), computed with openssl.
        assert_eq!(
            sign("secret", b"body"),
            "sha256=dc46983557fea127b43af721467eb9b3fde2338fe3e14f51952aa8478c13d355"
        );
    }

    #[test]
    fn sign_is_prefixed_and_hex() {
        let s = sign("k", b"payload");
        assert!(s.starts_with("sha256="));
        let hex_part = s.strip_prefix("sha256=").unwrap();
        assert_eq!(hex_part.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
