//! Host networking helpers for VM runtimes.
//!
//! Allocates a deterministic /30 subnet under `10.42.0.0/16` per VM instance
//! and the matching tap interface name, host-side IP, guest-side IP and MAC
//! address. The tap device itself is created by the hypervisor (Cloud
//! Hypervisor needs `CAP_NET_ADMIN` for that and already has it via
//! filesystem capabilities); Ring just decides on the parameters and feeds
//! them to the VMM and to cloud-init.
//!
//! The allocation is a pure function of the instance id, so two replicas of
//! the same deployment cannot collide and a Ring restart sees the same
//! mapping for a still-running VM.
//!
//! `10.42.0.0/16` carries 16384 /30 subnets — far more than any realistic
//! single-host CH workload. Hash collisions across the 14-bit space are
//! tolerated with best-effort: if two instance ids happen to hash to the
//! same /30, the second VM will fail to bring its tap up and Ring will
//! crashloop it. Documented as a known v1 limitation.

const TAP_PREFIX: &str = "ring-";
const SUBNET_BASE_HIGH: u8 = 10;
const SUBNET_BASE_MID: u8 = 42;

/// All the host-side network parameters Ring derives for a single VM.
#[derive(Debug, Clone)]
pub(crate) struct InstanceNet {
    /// Linux interface name. Capped at 15 chars (kernel limit IFNAMSIZ-1).
    pub tap_name: String,
    /// IP assigned to the host side of the tap (gateway, from the guest's POV).
    pub host_ip: String,
    /// IP the guest will configure on its eth0 via cloud-init.
    pub guest_ip: String,
    /// /30 mask, written long-form because cloud-init wants dotted form.
    pub netmask: String,
    /// CIDR prefix length, written `24`-style for `ip` invocations.
    pub prefix_len: u8,
    /// Locally administered MAC address (locally administered bit set, unicast).
    pub mac: String,
}

impl InstanceNet {
    /// Build the network parameters for a given VM instance id.
    pub fn for_instance(instance_id: &str) -> Self {
        // 14 bits of entropy → covers 16384 /30 subnets (10.42.0.0/16).
        let h = stable_hash(instance_id);
        let n = (h & 0x3fff) as u32;

        // /30 layout: .0 = network, .1 = host, .2 = guest, .3 = broadcast.
        let third = (n >> 6) as u8;
        let fourth_base = ((n & 0x3f) << 2) as u8; // multiple of 4
        let host_ip = format!(
            "{}.{}.{}.{}",
            SUBNET_BASE_HIGH,
            SUBNET_BASE_MID,
            third,
            fourth_base + 1
        );
        let guest_ip = format!(
            "{}.{}.{}.{}",
            SUBNET_BASE_HIGH,
            SUBNET_BASE_MID,
            third,
            fourth_base + 2
        );

        // 15 chars max for the tap name. Hex of the 14-bit slot fits in 4.
        let tap_name = format!("{}{:x}", TAP_PREFIX, n);

        // Locally-administered MAC: first byte is `02:` (bit 1 set, bit 0 clear
        // for unicast). The remaining 5 bytes are the low bits of the hash, so
        // the MAC is stable across boots of the same instance.
        let mac = format!(
            "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            (h >> 32) as u8,
            (h >> 24) as u8,
            (h >> 16) as u8,
            (h >> 8) as u8,
            h as u8,
        );

        Self {
            tap_name,
            host_ip,
            guest_ip,
            netmask: "255.255.255.252".to_string(),
            prefix_len: 30,
            mac,
        }
    }
}

/// Tiny FNV-1a 64-bit. Vendor-free, deterministic across builds. We don't
/// need cryptographic strength — just a good distribution across the /30
/// space.
fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_deterministic() {
        let a = InstanceNet::for_instance("ch-deadbeef-12345678");
        let b = InstanceNet::for_instance("ch-deadbeef-12345678");
        assert_eq!(a.tap_name, b.tap_name);
        assert_eq!(a.host_ip, b.host_ip);
        assert_eq!(a.guest_ip, b.guest_ip);
        assert_eq!(a.mac, b.mac);
    }

    #[test]
    fn distinct_instances_get_distinct_subnets() {
        // 5 random-looking ids should not collide on the /30 in practice.
        let ids = [
            "ch-aaaaaaaa-11111111",
            "ch-bbbbbbbb-22222222",
            "ch-cccccccc-33333333",
            "ch-dddddddd-44444444",
            "ch-eeeeeeee-55555555",
        ];
        let nets: Vec<_> = ids.iter().map(|i| InstanceNet::for_instance(i)).collect();
        for i in 0..nets.len() {
            for j in (i + 1)..nets.len() {
                assert_ne!(
                    nets[i].host_ip, nets[j].host_ip,
                    "{} and {} collided on the host IP",
                    ids[i], ids[j]
                );
            }
        }
    }

    #[test]
    fn ips_belong_to_the_same_slash_30() {
        let n = InstanceNet::for_instance("test");
        // Last octet of host_ip is 4k+1 and guest_ip is 4k+2.
        let host_last: u8 = n.host_ip.split('.').nth(3).unwrap().parse().unwrap();
        let guest_last: u8 = n.guest_ip.split('.').nth(3).unwrap().parse().unwrap();
        assert_eq!(guest_last, host_last + 1);
        assert_eq!(host_last % 4, 1);
    }

    #[test]
    fn tap_name_fits_in_ifnamsiz() {
        // IFNAMSIZ is 16 in the kernel; usable name length is 15.
        let n = InstanceNet::for_instance("ch-anything-anything-anything");
        assert!(
            n.tap_name.len() <= 15,
            "tap_name '{}' exceeds 15 chars",
            n.tap_name
        );
    }

    #[test]
    fn mac_is_locally_administered_unicast() {
        let n = InstanceNet::for_instance("test");
        // First byte must have bit 1 (locally administered) set and bit 0
        // (multicast) clear. `02` checks both: 0000_0010.
        let first: u8 = u8::from_str_radix(n.mac.split(':').next().unwrap(), 16).unwrap();
        assert_eq!(first & 0x03, 0x02);
    }

    #[test]
    fn prefix_len_matches_netmask() {
        let n = InstanceNet::for_instance("test");
        assert_eq!(n.prefix_len, 30);
        assert_eq!(n.netmask, "255.255.255.252");
    }
}
