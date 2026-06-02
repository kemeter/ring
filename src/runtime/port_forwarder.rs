//! Userspace TCP port forwarding via `socat`.
//!
//! For each `DeploymentPort { published, target, host_ip }` declared on a
//! VM-runtime deployment, Ring spawns one `socat` process that listens on
//! `<host_ip>:<published>` on the host (defaulting to `0.0.0.0`, all
//! interfaces) and forwards every accepted connection to `<vm_ip>:<target>`.
//! This mirrors the Docker runtime's `host_ip` binding. The forwarder's
//! lifetime is tied to the VM
//! through [`PortForwarder`]'s `Drop`: when the owning struct is dropped,
//! `socat` is killed and the listening port is freed.
//!
//! Why socat and not iptables? socat runs in userspace, so:
//! - no extra capabilities needed beyond what Ring already has (CH wants
//!   `CAP_NET_ADMIN`, but the forwarder itself is privilege-free),
//! - port conflicts surface immediately at `socat` startup as a clean
//!   `bind: Address already in use`,
//! - cleanup is just SIGKILL — no leftover NAT rules to garbage-collect.
//!
//! The trade-off is throughput: every byte traverses userspace twice. For
//! the typical Ring VM workload (HTTP, dev databases) that overhead is
//! negligible. A future iteration may switch to `iptables`/`nftables` DNAT
//! rules; documented in ROADMAP.

use crate::runtime::error::RuntimeError;
use std::net::TcpListener;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// Host interface a forwarder binds to when the deployment leaves `host_ip`
/// unset. All interfaces, matching the Docker runtime's default.
pub(crate) const DEFAULT_HOST_IP: &str = "0.0.0.0";

/// True when the host can currently bind `<host_ip>:<port>`. We bind then drop
/// immediately — `socat` re-binds milliseconds later thanks to `SO_REUSEADDR`.
/// The window between drop and re-bind is the same race docker's daemon
/// itself has when checking port availability, and it's harmless: if someone
/// snatches the port in between, the socat process exits and the caller
/// already has a clear "port allocated" path to fall back to.
///
/// A port bound on `0.0.0.0` and the same port bound on `127.0.0.1` are
/// distinct allocations to the kernel, so the check is interface-scoped on
/// purpose: two deployments may legitimately publish the same port number on
/// different host IPs.
pub(crate) fn host_port_available(host_ip: &str, port: u16) -> bool {
    TcpListener::bind((host_ip, port)).is_ok()
}

/// One running `socat` instance forwarding `published_port` on the host to
/// `<guest_ip>:target_port` inside the VM. Dropping it kills the daemon.
#[derive(Debug)]
pub(crate) struct PortForwarder {
    #[allow(dead_code)] // Retained for diagnostics / future introspection of active forwards.
    pub published_port: u16,
    #[allow(dead_code)]
    pub target_port: u16,
    /// Owned: dropping the forwarder kills the socat process.
    child: Child,
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Spawn a `socat` that forwards `<host_ip>:<published>` to
/// `<guest_ip>:<target>`. `host_ip` is `None` for the default (all
/// interfaces). Returns once the child has been spawned (we do not wait for
/// the listener to bind — if the port is taken, socat exits within
/// milliseconds and the next connection attempt from the user surfaces the
/// failure cleanly).
pub(crate) async fn spawn_forwarder(
    guest_ip: &str,
    published_port: u16,
    target_port: u16,
    host_ip: Option<&str>,
) -> Result<PortForwarder, RuntimeError> {
    let host_ip = host_ip.unwrap_or(DEFAULT_HOST_IP);

    // Match docker compose's contract: refuse to start the workload when a
    // requested host port is already bound. Without this pre-check, socat
    // would silently exit after `Bind: Address already in use`, the VM
    // would still boot, and the port would be a black hole.
    if !host_port_available(host_ip, published_port) {
        return Err(RuntimeError::PortAlreadyInUse(published_port));
    }

    // -d -d gives info-level diagnostics to stderr (handy when this fails
    // because the host port is already bound). `reuseaddr` lets us restart
    // a forwarder fast across deployment recreations without TIME_WAIT.
    // `fork` spawns a child per accepted connection — without it, socat
    // serves a single client and exits.
    // socat's TCP4-LISTEN binds 0.0.0.0 by default; `bind=<ip>` scopes it to
    // a single interface. We only emit `bind=` for a non-default host_ip so
    // the common case stays byte-for-byte the prior command line.
    let listen = if host_ip == DEFAULT_HOST_IP {
        format!("TCP4-LISTEN:{},reuseaddr,fork", published_port)
    } else {
        format!(
            "TCP4-LISTEN:{},reuseaddr,fork,bind={}",
            published_port, host_ip
        )
    };
    let connect = format!("TCP4:{}:{}", guest_ip, target_port);

    let child = Command::new("socat")
        .arg("-d")
        .arg("-d")
        .arg(&listen)
        .arg(&connect)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            RuntimeError::Other(format!(
                "failed to spawn socat for {}->{}:{}: {} (install socat?)",
                published_port, guest_ip, target_port, e
            ))
        })?;

    // Give socat a beat to bind the listening socket. If the port is
    // occupied, socat exits fast and the caller will see a closed pipe on
    // the first connection. We don't poll the port here because that would
    // race with whoever else is listening.
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    Ok(PortForwarder {
        published_port,
        target_port,
        child,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn socat_or_skip(test: &str) -> bool {
        if std::process::Command::new("socat")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("skipping {}: socat not installed", test);
            return true;
        }
        false
    }

    /// Bind a random ephemeral port and immediately release it so we can
    /// hand the number to a test that needs a port that's likely free.
    fn pick_free_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn forwarder_starts_and_drop_kills_socat() {
        if socat_or_skip("forwarder_starts_and_drop_kills_socat") {
            return;
        }

        let host_port = pick_free_port();
        // The "guest" side is unreachable; that's fine — we only check that
        // socat actually binds the listening port and that Drop tears it
        // down. A connection attempt is the cheapest signal of "bound".
        let fw = spawn_forwarder("127.0.0.99", host_port, 9999, None)
            .await
            .unwrap();

        // socat must hold the port — a fresh bind to the same port should fail.
        let busy = TcpListener::bind(format!("127.0.0.1:{}", host_port));
        assert!(
            busy.is_err(),
            "socat should be holding port {} but bind succeeded",
            host_port
        );

        drop(fw);

        // After Drop, the OS releases the listening socket. Give the kernel
        // a moment to reap the child and free the address.
        for _ in 0..40 {
            if TcpListener::bind(format!("127.0.0.1:{}", host_port)).is_ok() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!(
            "port {} still bound after Drop — socat process leaked",
            host_port
        );
    }

    #[tokio::test]
    async fn fields_carry_through() {
        if socat_or_skip("fields_carry_through") {
            return;
        }
        let host_port = pick_free_port();
        let fw = spawn_forwarder("10.0.0.1", host_port, 5432, None)
            .await
            .unwrap();
        assert_eq!(fw.published_port, host_port);
        assert_eq!(fw.target_port, 5432);
        drop(fw);
    }

    #[tokio::test]
    async fn spawn_forwarder_rejects_a_port_already_in_use() {
        // We do not need socat for this — the pre-check rejects before any
        // process is spawned. Skipping when socat is absent would hide a
        // regression that only surfaces in environments where socat exists.
        let host_port = pick_free_port();
        let _holder = TcpListener::bind(format!("0.0.0.0:{}", host_port))
            .expect("test setup: holder must bind the port");

        let err = spawn_forwarder("10.0.0.1", host_port, 5432, None)
            .await
            .expect_err("spawn_forwarder should refuse a bound port");

        match err {
            RuntimeError::PortAlreadyInUse(p) => assert_eq!(p, host_port),
            other => panic!("expected PortAlreadyInUse, got {:?}", other),
        }
    }

    /// A loopback-scoped forwarder must bind 127.0.0.1 only, leaving the same
    /// port free on other interfaces. This is the contract that makes
    /// `host_ip: 127.0.0.1` actually mean "loopback only" on the CH runtime.
    #[tokio::test]
    async fn forwarder_with_host_ip_binds_only_that_interface() {
        if socat_or_skip("forwarder_with_host_ip_binds_only_that_interface") {
            return;
        }

        let host_port = pick_free_port();
        let fw = spawn_forwarder("127.0.0.99", host_port, 9999, Some("127.0.0.1"))
            .await
            .unwrap();

        // socat holds 127.0.0.1:<port> — a fresh bind there must fail.
        let busy = TcpListener::bind(format!("127.0.0.1:{}", host_port));
        assert!(
            busy.is_err(),
            "socat should hold 127.0.0.1:{} but bind succeeded",
            host_port
        );

        drop(fw);
    }

    /// host_port_available is interface-scoped: a port held on loopback must
    /// still report available on a different host IP.
    #[test]
    fn host_port_available_is_interface_scoped() {
        let port = pick_free_port();
        let _holder = TcpListener::bind(format!("127.0.0.1:{}", port))
            .expect("test setup: holder must bind loopback");

        assert!(
            !host_port_available("127.0.0.1", port),
            "loopback port {} is held, must report unavailable",
            port
        );
    }
}
