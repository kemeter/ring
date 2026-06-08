//! Outbound NAT for VM guests.
//!
//! A microVM sits on a private /30 under `10.42.0.0/16` (see [`super::host_net`]).
//! Ring gives it an IP, a route and a tap — but that only lets the guest reach
//! the host. For the guest to reach the *Internet* (git clone, composer, apt…)
//! the host must masquerade the guest subnet, exactly like Docker does for its
//! bridge. Without this the guest resolves nothing and every outbound call dies
//! with "Could not resolve host".
//!
//! This is the host-side counterpart to tap creation, and like it, it's Ring's
//! job — never the operator's. We enable IPv4 forwarding and install a single
//! idempotent MASQUERADE rule covering the whole `10.42.0.0/16` range (one rule
//! for every VM, present or future), so it's set up once and costs nothing to
//! re-assert on later boots.
//!
//! Implemented by shelling out to `iptables`: unlike `ip tuntap` (which needs
//! the *ambient* capability set to work in a child process), iptables performs
//! its netfilter syscalls itself and inherits ring-server's effective
//! `CAP_NET_ADMIN`. Failures are logged, not fatal — a VM that can't reach the
//! Internet still boots and serves inbound traffic.

use std::process::Command;
use std::sync::Once;
use tracing::{info, warn};

/// The guest supernet every per-instance /30 is carved from.
const GUEST_SUPERNET: &str = "10.42.0.0/16";

static ENSURE_ONCE: Once = Once::new();

/// Ensure outbound NAT for the guest supernet is in place. Idempotent and
/// cheap; safe to call on every VM start. Runs its actual work only once per
/// ring-server process (the rules are global, not per-VM).
pub(crate) fn ensure_outbound_nat() {
    ENSURE_ONCE.call_once(|| {
        // iptables shells out, and the nf_tables backend needs CAP_NET_ADMIN in
        // the *ambient* set to work in that child — a child does NOT inherit the
        // parent's permitted/effective caps (same gotcha tap.rs documents for
        // `ip`). setcap only fills permitted/effective, so we raise NET_ADMIN
        // into the ambient set here, once, so the forked iptables inherits it.
        if let Err(e) = raise_ambient_net_admin() {
            warn!("could not raise ambient CAP_NET_ADMIN: {e} (iptables may be denied)");
        }
        if let Err(e) = enable_ip_forward() {
            warn!("could not enable net.ipv4.ip_forward: {e} (guest outbound may fail)");
        }
        if let Err(e) = ensure_masquerade() {
            warn!(
                "could not install MASQUERADE for {GUEST_SUPERNET}: {e} \
                 (guest Internet access may fail; ring-server needs CAP_NET_ADMIN)"
            );
        } else {
            info!("outbound NAT ready for guest subnet {GUEST_SUPERNET}");
        }
    });
}

/// Raise CAP_NET_ADMIN into the ambient set so child processes (iptables)
/// inherit it. Ambient requires the cap to be in BOTH permitted AND inheritable.
/// `setcap cap_net_admin+ep` only fills permitted/effective — NOT inheritable —
/// so we first add NET_ADMIN to the inheritable set via capset(), then raise it
/// into ambient. If the cap isn't permitted at all (no setcap, not root), this
/// fails and NAT just won't apply.
fn raise_ambient_net_admin() -> Result<(), String> {
    const PR_CAP_AMBIENT: libc::c_int = 47;
    const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;
    const CAP_NET_ADMIN: u32 = 12;
    const CAP_NET_ADMIN_BIT: u32 = 1 << CAP_NET_ADMIN; // NET_ADMIN is < 32

    // 1) Add NET_ADMIN to the inheritable set (keeping permitted/effective).
    //    Use the v3 capability ABI (_LINUX_CAPABILITY_VERSION_3).
    const VERSION_3: u32 = 0x2008_0522;
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: libc::c_int,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    let mut hdr = CapHeader {
        version: VERSION_3,
        pid: 0,
    };
    let mut data = [CapData::default(); 2]; // two u32 blocks for 64 caps

    // SAFETY: capget/capset are the documented syscalls; structs match the v3 ABI.
    let rc = unsafe { libc::syscall(libc::SYS_capget, &mut hdr, data.as_mut_ptr()) };
    if rc != 0 {
        return Err(format!(
            "capget failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // NET_ADMIN (12) lives in block 0. Add it to inheritable; require permitted.
    data[0].inheritable |= CAP_NET_ADMIN_BIT;
    let rc = unsafe { libc::syscall(libc::SYS_capset, &mut hdr, data.as_ptr()) };
    if rc != 0 {
        return Err(format!(
            "capset (inheritable) failed: {} — is cap_net_admin in the permitted set? (setcap cap_net_admin+ep)",
            std::io::Error::last_os_error()
        ));
    }

    // 2) Raise it into the ambient set so forked children keep it.
    // SAFETY: prctl PR_CAP_AMBIENT is a pure capability-set op; args are scalars.
    let rc = unsafe {
        libc::prctl(
            PR_CAP_AMBIENT,
            PR_CAP_AMBIENT_RAISE,
            CAP_NET_ADMIN as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

fn enable_ip_forward() -> std::io::Result<()> {
    // Best-effort: write the sysctl directly so we don't depend on `sysctl`.
    std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")
}

fn ensure_masquerade() -> Result<(), String> {
    // Per the Firecracker network-setup guide, NAT alone is NOT enough: with
    // Docker (or any FORWARD policy = DROP) installed, guest->Internet packets
    // get dropped at the forward stage. Three rules are needed:
    //   1. nat POSTROUTING ... MASQUERADE        — rewrite guest src to host IP
    //   2. FORWARD conntrack RELATED,ESTABLISHED — let return traffic back in
    //   3. FORWARD -s guest ... ACCEPT           — let guest traffic out
    // We scope by subnet (not tap name) so one set of rules covers every VM.
    let out_iface = default_out_iface();

    // Rule 1: masquerade. Pin the output interface when we found one (matches
    // the guide's `-o eth0`); otherwise masquerade any non-guest destination.
    let mut masq: Vec<String> = vec![
        "-t".into(),
        "nat".into(),
        "POSTROUTING".into(),
        "-s".into(),
        GUEST_SUPERNET.into(),
        "!".into(),
        "-d".into(),
        GUEST_SUPERNET.into(),
    ];
    if let Some(ref dev) = out_iface {
        masq.push("-o".into());
        masq.push(dev.clone());
    }
    masq.extend(["-j".into(), "MASQUERADE".into()]);

    // Rule 2: accept established/related return traffic.
    let fwd_back: Vec<String> = vec![
        "FORWARD".into(),
        "-m".into(),
        "conntrack".into(),
        "--ctstate".into(),
        "RELATED,ESTABLISHED".into(),
        "-j".into(),
        "ACCEPT".into(),
    ];

    // Rule 3: accept outbound traffic from the guest subnet.
    let fwd_out: Vec<String> = vec![
        "FORWARD".into(),
        "-s".into(),
        GUEST_SUPERNET.into(),
        "-j".into(),
        "ACCEPT".into(),
    ];

    ensure_rule(&masq)?;
    ensure_rule(&fwd_back)?;
    ensure_rule(&fwd_out)?;
    Ok(())
}

/// Add an iptables rule if it isn't already present. `rule` is the rule spec
/// (chain + matchers + target) WITHOUT the leading `-A`/`-C` verb. We probe
/// with `-C` and only `-A` when absent, so repeated calls never duplicate it.
fn ensure_rule(rule: &[String]) -> Result<(), String> {
    let as_str: Vec<&str> = rule.iter().map(|s| s.as_str()).collect();

    if run_iptables(&with_verb(&as_str, "-C")).unwrap_or(false) {
        return Ok(());
    }
    if run_iptables(&with_verb(&as_str, "-A"))? {
        Ok(())
    } else {
        Err(format!("iptables -A failed for rule {:?}", rule))
    }
}

/// Insert the verb (`-A`/`-C`) right after a leading `-t <table>` if present,
/// else at the front. iptables wants `-t nat -A CHAIN ...`, not `-A -t nat ...`.
fn with_verb<'a>(rule: &[&'a str], verb: &'a str) -> Vec<&'a str> {
    let mut out = Vec::with_capacity(rule.len() + 1);
    if rule.first() == Some(&"-t") && rule.len() >= 2 {
        out.push(rule[0]);
        out.push(rule[1]);
        out.push(verb);
        out.extend_from_slice(&rule[2..]);
    } else {
        out.push(verb);
        out.extend_from_slice(rule);
    }
    out
}

/// The host interface that carries the default route (the way out to the
/// Internet). `None` if it can't be determined — then MASQUERADE isn't pinned
/// to an interface, which still works for a single-uplink host.
fn default_out_iface() -> Option<String> {
    let out = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "default via 192.168.1.1 dev eth0 ..." → take the token after `dev`.
    let mut it = text.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "dev" {
            return it.next().map(|s| s.to_string());
        }
    }
    None
}

/// Resolve the iptables binary. ring-server may run with a minimal PATH that
/// omits /usr/sbin (where iptables lives on most distros), so `Command::new
/// ("iptables")` can fail with ENOENT even though it's installed. Probe the
/// usual locations and fall back to the bare name (PATH) if none match.
fn iptables_bin() -> String {
    for p in ["/usr/sbin/iptables", "/sbin/iptables", "/usr/bin/iptables"] {
        if std::path::Path::new(p).exists() {
            return p.to_string();
        }
    }
    "iptables".to_string()
}

/// Run `iptables` with the given args. Returns Ok(true) on success (exit 0),
/// Ok(false) on a clean non-zero exit, Err on spawn failure (binary missing).
/// A non-zero `-A` exit logs stderr so the operator can see why NAT didn't apply.
fn run_iptables(args: &[&str]) -> Result<bool, String> {
    let out = Command::new(iptables_bin())
        .args(args)
        .output()
        .map_err(|e| format!("could not run iptables: {e} (is it installed?)"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let trimmed = stderr.trim();
        if !trimmed.is_empty() {
            warn!("iptables {:?} failed: {trimmed}", args);
        }
    }
    Ok(out.status.success())
}
