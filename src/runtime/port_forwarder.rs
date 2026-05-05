//! Userspace TCP port forwarding via `socat`.
//!
//! For each `DeploymentPort { published, target }` declared on a VM-runtime
//! deployment, Ring spawns one `socat` process that listens on
//! `0.0.0.0:<published>` on the host and forwards every accepted connection
//! to `<vm_ip>:<target>`. The lifetime of each forwarder is tied to the VM
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
use std::process::Stdio;
use tokio::process::{Child, Command};

/// One running `socat` instance forwarding `published_port` on the host to
/// `<guest_ip>:target_port` inside the VM. Dropping it kills the daemon.
pub(crate) struct PortForwarder {
    pub published_port: u16,
    pub target_port: u16,
    /// Owned: dropping the forwarder kills the socat process.
    child: Child,
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Spawn a `socat` that forwards `0.0.0.0:<published>` to `<guest_ip>:<target>`.
/// Returns once the child has been spawned (we do not wait for the listener
/// to bind — if the port is taken, socat exits within milliseconds and the
/// next connection attempt from the user surfaces the failure cleanly).
pub(crate) async fn spawn_forwarder(
    guest_ip: &str,
    published_port: u16,
    target_port: u16,
) -> Result<PortForwarder, RuntimeError> {
    // -d -d gives info-level diagnostics to stderr (handy when this fails
    // because the host port is already bound). `reuseaddr` lets us restart
    // a forwarder fast across deployment recreations without TIME_WAIT.
    // `fork` spawns a child per accepted connection — without it, socat
    // serves a single client and exits.
    let listen = format!("TCP4-LISTEN:{},reuseaddr,fork", published_port);
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
        let fw = spawn_forwarder("127.0.0.99", host_port, 9999)
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
        let fw = spawn_forwarder("10.0.0.1", host_port, 5432).await.unwrap();
        assert_eq!(fw.published_port, host_port);
        assert_eq!(fw.target_port, 5432);
        drop(fw);
    }
}
