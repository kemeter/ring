//! Runtime-agnostic TCP and HTTP health probes.
//!
//! Each runtime resolves an `IpAddr` for a given instance via
//! [`RuntimeLifecycle::instance_address`]; once that's done the actual probe
//! is identical regardless of whether the workload is a Docker container, a
//! Cloud Hypervisor VM, or a future Firecracker microVM. Keeping the probe
//! logic here means we don't reimplement TCP connect / HTTP GET semantics
//! per runtime.

use crate::models::health_check::HealthCheckStatus;
use std::net::IpAddr;
use std::time::Duration;
use tokio::net::TcpStream;

/// Open a TCP connection to `(ip, port)`, bounded by `timeout`.
///
/// Success means the kernel accepted the SYN — nothing is sent or read on
/// the socket. Mirrors the historical Docker-runtime probe.
pub(crate) async fn tcp_probe(
    ip: IpAddr,
    port: u16,
    timeout: Duration,
) -> (HealthCheckStatus, Option<String>) {
    let addr = format!("{}:{}", ip, port);

    match tokio::time::timeout(timeout, TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => (
            HealthCheckStatus::Success,
            Some(format!("TCP connection to {} successful", addr)),
        ),
        Ok(Err(e)) => (
            HealthCheckStatus::Failed,
            Some(format!("TCP connection failed: {}", e)),
        ),
        Err(_) => (
            HealthCheckStatus::Failed,
            Some(format!("TCP connection timed out for {}", addr)),
        ),
    }
}

/// Issue an HTTP GET against `url`, expecting a 2xx response.
///
/// `localhost` in the URL is rewritten to `ip` so that operators can
/// declare probes the same way they'd write them for a deployment that
/// listens locally inside the container/VM.
pub(crate) async fn http_probe(
    ip: IpAddr,
    url: &str,
    timeout: Duration,
) -> (HealthCheckStatus, Option<String>) {
    let target_url = url.replace("localhost", &ip.to_string());

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => {
            return (
                HealthCheckStatus::Failed,
                Some(format!("Failed to create HTTP client: {}", e)),
            );
        }
    };

    match client.get(&target_url).send().await {
        Ok(response) => {
            let code = response.status().as_u16();
            if (200..300).contains(&code) {
                (
                    HealthCheckStatus::Success,
                    Some(format!(
                        "HTTP check successful ({}) for {}",
                        code, target_url
                    )),
                )
            } else {
                (
                    HealthCheckStatus::Failed,
                    Some(format!(
                        "HTTP check failed with status {} for {}",
                        code, target_url
                    )),
                )
            }
        }
        Err(e) => (
            HealthCheckStatus::Failed,
            Some(format!("HTTP request failed for {}: {}", target_url, e)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn tcp_probe_succeeds_against_listening_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let (status, _msg) = tcp_probe(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            Duration::from_secs(2),
        )
        .await;

        assert!(matches!(status, HealthCheckStatus::Success));
    }

    #[tokio::test]
    async fn tcp_probe_fails_when_port_closed() {
        // Bind then drop, then try to connect — the port is now free.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };

        let (status, msg) = tcp_probe(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            Duration::from_millis(500),
        )
        .await;

        assert!(matches!(status, HealthCheckStatus::Failed));
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn http_probe_succeeds_against_2xx_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Tiny one-shot HTTP server: accept one connection, write 200 OK.
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let url = format!("http://localhost:{}/", port);
        let (status, _msg) = http_probe(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &url,
            Duration::from_secs(2),
        )
        .await;

        assert!(matches!(status, HealthCheckStatus::Success));
    }

    #[tokio::test]
    async fn http_probe_fails_against_5xx_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let _ = socket
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .await;
            }
        });

        let url = format!("http://localhost:{}/", port);
        let (status, msg) = http_probe(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &url,
            Duration::from_secs(2),
        )
        .await;

        assert!(matches!(status, HealthCheckStatus::Failed));
        assert!(msg.unwrap().contains("503"));
    }
}
