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
//! The same pass isolates the guests (see [`ensure_guest_isolation`]): a guest
//! reaches the Internet, but not another guest, the host, or any private or
//! link-local range. [`guard_tap_source`] adds per-tap anti-spoofing.
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

/// Every guest tap is named `ring-<slot>` (see [`super::host_net`]).
const GUEST_IFACES: &str = "ring-+";

/// Chain filtering what guests send through the host (FORWARD).
const FORWARD_CHAIN: &str = "RING-GUEST-FORWARD";

/// Chain filtering what guests send to the host itself (INPUT).
const INPUT_CHAIN: &str = "RING-GUEST-INPUT";

/// Destinations a guest must not reach: the other guests (inside 10/8), the
/// host's private networks and link-local addresses, cloud metadata included.
const BLOCKED_RANGES: [&str; 4] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
];

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
        if let Err(e) = ensure_guest_isolation() {
            warn!(
                "could not isolate the guest network: {e} \
                 (guests may reach each other and the host's private networks)"
            );
        } else {
            info!("guest network isolation ready for {GUEST_IFACES}");
        }
    });
}

/// Install the chains that isolate guests, and jump to them from the top of
/// FORWARD and INPUT for traffic entering from a guest tap.
///
/// A guest may:
/// - get the replies of connections the host opened to it (health checks,
///   published ports) and of its own outbound connections;
/// - resolve names (port 53), wherever its resolver is: guest images often
///   point at a resolver on the host's LAN;
/// - reach any public address.
///
/// Everything else from a guest is dropped: a source outside the guest
/// supernet (spoofing), a private or link-local destination, and any new
/// connection to the host itself.
///
/// The chains are flushed and refilled, so a server restart applies the rules
/// of its own version.
fn ensure_guest_isolation() -> Result<(), String> {
    for (chain, rules) in [
        (FORWARD_CHAIN, forward_rules()),
        (INPUT_CHAIN, input_rules()),
    ] {
        reset_chain(chain)?;
        for rule in rules {
            let mut args = vec!["-A", chain];
            args.extend(rule.iter().copied());
            if !run_iptables(&args)? {
                return Err(format!("iptables -A {chain} failed for {:?}", rule));
            }
        }
    }
    ensure_jump("FORWARD", FORWARD_CHAIN)?;
    ensure_jump("INPUT", INPUT_CHAIN)?;
    Ok(())
}

fn forward_rules() -> Vec<Vec<&'static str>> {
    let mut rules = vec![
        vec![
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "RETURN",
        ],
        vec!["!", "-s", GUEST_SUPERNET, "-j", "DROP"],
        vec!["-p", "udp", "--dport", "53", "-j", "RETURN"],
        vec!["-p", "tcp", "--dport", "53", "-j", "RETURN"],
    ];
    for range in BLOCKED_RANGES {
        rules.push(vec!["-d", range, "-j", "DROP"]);
    }
    rules
}

fn input_rules() -> Vec<Vec<&'static str>> {
    vec![
        vec![
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
            "-j",
            "RETURN",
        ],
        vec!["-j", "DROP"],
    ]
}

/// Create `chain` if it does not exist, and empty it.
fn reset_chain(chain: &str) -> Result<(), String> {
    if !run_iptables_quiet(&["-L", chain, "-n"])? && !run_iptables(&["-N", chain])? {
        return Err(format!("iptables -N {chain} failed"));
    }
    if !run_iptables(&["-F", chain])? {
        return Err(format!("iptables -F {chain} failed"));
    }
    Ok(())
}

/// Jump from the top of `from` to `chain` for traffic entering from a guest
/// tap, unless that jump is already there.
fn ensure_jump(from: &str, chain: &str) -> Result<(), String> {
    let rule = ["-i", GUEST_IFACES, "-j", chain];
    let mut check = vec!["-C", from];
    check.extend(rule);
    if run_iptables_quiet(&check)? {
        return Ok(());
    }
    let mut insert = vec!["-I", from, "1"];
    insert.extend(rule);
    if run_iptables(&insert)? {
        Ok(())
    } else {
        Err(format!("iptables -I {from} failed for the jump to {chain}"))
    }
}

/// Drop what a guest sends from another address than the one routed to its
/// tap, so it cannot pose as another guest. The guest supernet is routed per
/// /30, one tap each, so strict reverse-path filtering does exactly that.
/// Call it once the tap exists and before the guest boots.
pub(crate) fn guard_tap_source(tap_name: &str) {
    let path = format!("/proc/sys/net/ipv4/conf/{tap_name}/rp_filter");
    if let Err(e) = std::fs::write(&path, "1") {
        warn!(
            "could not enable reverse-path filtering on {tap_name}: {e} (the guest may spoof its source address)"
        );
    }
}

/// Raise CAP_NET_ADMIN into the ambient set so child processes (iptables)
/// inherit it. Ambient requires the cap to be in BOTH permitted AND inheritable.
/// `setcap cap_net_admin+ep` only fills permitted/effective — NOT inheritable —
/// so we first add NET_ADMIN to the inheritable set, then raise it into ambient.
/// If the cap isn't permitted at all (no setcap, not root), this fails and NAT
/// just won't apply. Uses the safe `caps` wrapper (no manual FFI/capset structs).
fn raise_ambient_net_admin() -> Result<(), String> {
    use caps::{CapSet, Capability};

    caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_ADMIN).map_err(|e| {
        format!(
            "could not add CAP_NET_ADMIN to the inheritable set: {e} \
             — is it in the permitted set? (setcap cap_net_admin+ep $(command -v ring))"
        )
    })?;
    caps::raise(None, CapSet::Ambient, Capability::CAP_NET_ADMIN)
        .map_err(|e| format!("could not raise CAP_NET_ADMIN into the ambient set: {e}"))?;
    Ok(())
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
    parse_default_iface(&String::from_utf8_lossy(&out.stdout))
}

/// Parse the interface name from `ip route show default` output: from the line
/// that starts with `default`, take the token after `dev`. Split out from
/// [`default_out_iface`] so it can be unit-tested without invoking `ip`. Only
/// the default route counts — a non-default line that happens to carry `dev`
/// (e.g. a link-scope route) must not be mistaken for the uplink.
fn parse_default_iface(route_output: &str) -> Option<String> {
    for line in route_output.lines() {
        if line.split_whitespace().next() != Some("default") {
            continue;
        }
        let mut it = line.split_whitespace();
        while let Some(tok) = it.next() {
            if tok == "dev" {
                return it.next().map(|s| s.to_string());
            }
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

/// Like [`run_iptables`], for probes whose failure is an expected answer
/// (`-C` on a missing rule, `-L` on a missing chain): nothing is logged.
fn run_iptables_quiet(args: &[&str]) -> Result<bool, String> {
    Command::new(iptables_bin())
        .args(args)
        .output()
        .map(|out| out.status.success())
        .map_err(|e| format!("could not run iptables: {e} (is it installed?)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_rules_let_replies_and_dns_through_before_the_drops() {
        let rules = forward_rules();
        let first_drop = rules.iter().position(|r| r.ends_with(&["DROP"])).unwrap();
        assert_eq!(
            rules[0][..4],
            ["-m", "conntrack", "--ctstate", "RELATED,ESTABLISHED"]
        );
        assert_eq!(rules[first_drop], ["!", "-s", GUEST_SUPERNET, "-j", "DROP"]);
        let dns = rules.iter().rposition(|r| r.contains(&"53")).unwrap();
        let first_destination_drop = rules.iter().position(|r| r[0] == "-d").unwrap();
        assert!(
            dns < first_destination_drop,
            "DNS must be allowed before the range drops"
        );
    }

    #[test]
    fn forward_rules_drop_every_private_and_link_local_range() {
        let rules = forward_rules();
        for range in [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "169.254.0.0/16",
        ] {
            assert!(
                rules.contains(&vec!["-d", range, "-j", "DROP"]),
                "{range} is not dropped"
            );
        }
        // The guest supernet, other guests included, sits inside 10/8.
        assert!(GUEST_SUPERNET.starts_with("10."));
    }

    #[test]
    fn input_rules_only_let_replies_reach_the_host() {
        assert_eq!(
            input_rules(),
            vec![
                vec![
                    "-m",
                    "conntrack",
                    "--ctstate",
                    "RELATED,ESTABLISHED",
                    "-j",
                    "RETURN"
                ],
                vec!["-j", "DROP"],
            ]
        );
    }

    #[test]
    fn with_verb_inserts_after_table_for_nat_rules() {
        // `-t nat <chain> ...` must become `-t nat -A <chain> ...`, never
        // `-A -t nat ...` (iptables rejects the latter).
        let rule = [
            "-t",
            "nat",
            "POSTROUTING",
            "-s",
            GUEST_SUPERNET,
            "-j",
            "MASQUERADE",
        ];
        assert_eq!(
            with_verb(&rule, "-A"),
            vec![
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-s",
                GUEST_SUPERNET,
                "-j",
                "MASQUERADE"
            ]
        );
    }

    #[test]
    fn with_verb_prepends_for_filter_rules() {
        // A FORWARD rule has no leading `-t`, so the verb goes at the front.
        let rule = ["FORWARD", "-s", GUEST_SUPERNET, "-j", "ACCEPT"];
        assert_eq!(
            with_verb(&rule, "-C"),
            vec!["-C", "FORWARD", "-s", GUEST_SUPERNET, "-j", "ACCEPT"]
        );
    }

    #[test]
    fn parse_default_iface_extracts_dev() {
        assert_eq!(
            parse_default_iface("default via 192.168.1.1 dev wlp3s0 proto dhcp metric 600\n")
                .as_deref(),
            Some("wlp3s0")
        );
    }

    #[test]
    fn parse_default_iface_handles_eth_and_extra_fields() {
        assert_eq!(
            parse_default_iface("default via 10.0.0.1 dev eth0 onlink").as_deref(),
            Some("eth0")
        );
    }

    #[test]
    fn parse_default_iface_none_when_no_default_route() {
        assert_eq!(parse_default_iface(""), None);
        assert_eq!(parse_default_iface("10.0.0.0/24 dev eth0 scope link"), None);
    }

    #[test]
    fn iptables_bin_falls_back_to_bare_name() {
        // The result is always a non-empty command; on a host without any of the
        // probed absolute paths it must fall back to "iptables" (PATH lookup).
        let bin = iptables_bin();
        assert!(!bin.is_empty());
        assert!(bin.ends_with("iptables"));
    }

    #[test]
    fn guest_supernet_is_the_documented_range() {
        // The /16 the per-instance /30s are carved from; the NAT rules scope to it.
        assert_eq!(GUEST_SUPERNET, "10.42.0.0/16");
    }
}
