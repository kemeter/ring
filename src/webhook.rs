//! Webhook delivery.
//!
//! Turns a queued event into a signed HTTP POST to a subscriber. Called by the
//! event worker, never inline in the scheduler — delivery latency and failures
//! must not touch the reconciliation loop.
//!
//! The body is the event's JSON payload. When the subscriber has a secret, the
//! POST carries an `X-Ring-Signature: sha256=<hex>` header (HMAC-SHA256 of the
//! body), the GitHub/Stripe convention, so the receiver can authenticate it.

use crate::models::webhook::{Webhook, is_blocked_ip, url_safety_violation};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Resolves a host name with the system resolver (`getaddrinfo`, via tokio).
struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            // The port is a placeholder: reqwest overrides it with the URL's.
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Wraps a resolver and refuses a name if ANY address it resolves to is one a
/// subscriber must never reach (see `is_blocked_ip`).
///
/// Creation only checks the URL's text, so a hostname pointing at loopback or
/// link-local would otherwise pass. Vetting here, inside the client, binds the
/// connection to the addresses that were checked: reqwest connects to what
/// this returns and never resolves the name a second time, so a DNS answer
/// that changes between check and connect cannot slip through. Refusing on any
/// blocked address, rather than dropping it, keeps a name that mixes public and
/// internal records from reaching the internal one on a retry.
struct GuardedResolver<R> {
    inner: R,
}

impl<R: Resolve + 'static> Resolve for GuardedResolver<R> {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let resolving = self.inner.resolve(name);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = resolving.await?.collect();
            if let Some(blocked) = addrs.iter().find(|addr| is_blocked_ip(&addr.ip())) {
                return Err(format!(
                    "{host} resolves to an internal address ({}), refusing to connect",
                    blocked.ip()
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Build a delivery client resolving names through `resolver`, vetted by
/// [`GuardedResolver`].
///
/// Redirects are disabled (`Policy::none()`): a subscriber URL is user-supplied
/// and following a 3xx would let it bounce the server-side request to an
/// internal address (e.g. cloud metadata at 169.254.169.254), defeating the
/// host allowlist enforced at creation. A redirecting subscriber just fails
/// delivery and is retried/dead-lettered like any other non-2xx.
///
/// Proxies are disabled too: through a proxy the target name is resolved by the
/// proxy, never by this client, which would bypass the resolver check.
fn build_client<R: Resolve + 'static>(resolver: R) -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver { inner: resolver }))
        .build()
        .expect("failed to build webhook delivery client")
}

/// Shared delivery client, built once. Reusing it keeps the connection pool and
/// TLS config warm across deliveries instead of rebuilding both on every POST.
pub(crate) fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build_client(SystemResolver))
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

/// Deliver `body` (the event payload JSON) to `hook` with `client` (the
/// guarded [`client`] outside tests). Returns `Ok(())` on a 2xx, `Err(reason)`
/// otherwise (non-2xx, refused target or transport error) so the worker can
/// decide whether to retry or dead-letter. Never panics.
pub(crate) async fn deliver(
    client: &reqwest::Client,
    hook: &Webhook,
    kind: &str,
    body: &[u8],
) -> Result<(), String> {
    // Re-check the URL at delivery, not only at creation: an IP-literal host is
    // never handed to the resolver, and a subscriber stored before a rule was
    // tightened must not keep bypassing it.
    if let Some(reason) = url_safety_violation(&hook.url) {
        return Err(format!("refusing to deliver to {}: {}", hook.url, reason));
    }

    let mut request = client
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
    use super::{GuardedResolver, build_client, deliver, sign};
    use crate::models::webhook::Webhook;
    use reqwest::dns::{Addrs, Name, Resolve, Resolving};
    use std::net::{IpAddr, SocketAddr};
    use std::str::FromStr;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// Resolves every name to a fixed set of addresses, standing in for a DNS
    /// record the subscriber controls.
    struct FixedResolver(Vec<IpAddr>);

    impl Resolve for FixedResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            let addrs: Vec<SocketAddr> = self.0.iter().map(|ip| SocketAddr::new(*ip, 0)).collect();
            Box::pin(async move { Ok(Box::new(addrs.into_iter()) as Addrs) })
        }
    }

    fn hook(url: String) -> Webhook {
        Webhook {
            id: "w".into(),
            url,
            secret: None,
            events: vec![],
            created_at: "2026-01-01T00:00:00Z".into(),
            revoked_at: None,
        }
    }

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|ip| ip.parse().unwrap()).collect()
    }

    async fn guarded(list: &[&str]) -> Result<Vec<SocketAddr>, String> {
        let resolver = GuardedResolver {
            inner: FixedResolver(ips(list)),
        };
        resolver
            .resolve(Name::from_str("subscriber.example").unwrap())
            .await
            .map(|addrs| addrs.collect())
            .map_err(|e| e.to_string())
    }

    /// Assert nothing connected to `listener` within a short window.
    async fn assert_no_connection(listener: &TcpListener) {
        let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "the subscriber must never be connected to"
        );
    }

    #[tokio::test]
    async fn a_name_resolving_to_loopback_is_refused_before_connecting() {
        // An ordinary-looking name passes the creation-time URL check; what it
        // resolves to must be vetted before any connection is opened.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = build_client(FixedResolver(ips(&["127.0.0.1"])));

        let result = deliver(
            &client,
            &hook(format!("http://subscriber.example:{port}/hook")),
            "deployment.created",
            b"{}",
        )
        .await;

        assert!(result.is_err());
        assert_no_connection(&listener).await;
    }

    #[tokio::test]
    async fn an_ip_literal_is_rechecked_at_delivery() {
        // An IP-literal host never reaches the resolver, so a subscriber stored
        // before the URL rules covered it must be refused by the delivery-time
        // re-check.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = build_client(FixedResolver(vec![]));

        for host in ["127.0.0.1", "[::ffff:127.0.0.1]"] {
            let result = deliver(
                &client,
                &hook(format!("http://{host}:{port}/hook")),
                "deployment.created",
                b"{}",
            )
            .await;
            assert!(result.is_err(), "{host} must be refused");
        }
        assert_no_connection(&listener).await;
    }

    #[tokio::test]
    async fn resolved_addresses_are_vetted() {
        assert!(guarded(&["127.0.0.1"]).await.is_err());
        assert!(guarded(&["169.254.169.254"]).await.is_err());
        assert!(guarded(&["::1"]).await.is_err());
        assert!(guarded(&["::ffff:127.0.0.1"]).await.is_err());
        assert!(guarded(&["fe80::1"]).await.is_err());
        // One internal record is enough to refuse the whole name.
        assert!(guarded(&["93.184.216.34", "127.0.0.1"]).await.is_err());

        // Public and private addresses pass through untouched.
        let allowed = guarded(&["93.184.216.34", "10.0.0.5", "fc00::1"])
            .await
            .unwrap();
        assert_eq!(allowed.len(), 3);
    }

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
