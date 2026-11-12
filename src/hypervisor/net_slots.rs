//! Collision-free allocation of microVM network slots.
//!
//! Each guest gets one of the 16384 /30 subnets of `10.42.0.0/16`, and the
//! tap named after it (see [`super::host_net`]). The slot used to be a hash of
//! the instance id alone, so two instances could land on the same one: on
//! Cloud Hypervisor they silently shared a tap and a host IP, on Firecracker
//! the second one failed to boot.
//!
//! A slot is now claimed before the guest boots, and the claim is kept on disk
//! so a restarted server finds every running guest's slot again:
//!
//! - `slots/<slot>` holds the id of the instance that claimed it. It is created
//!   with `O_EXCL`, so two concurrent claims of one slot cannot both succeed;
//! - `instances/<instance_id>` holds the slot, for lookups by instance.
//!
//! The claim starts at the slot the instance id hashes to and moves to the
//! next free one. A slot is taken while it holds a claim or while its tap
//! exists on the host, which covers the guests booted by a version of Ring
//! without claims. An instance without a claim keeps its hashed slot, so those
//! guests keep their network across the upgrade.
//!
//! A claim is released when its instance is stopped. One left behind by a
//! boot that failed is taken over once its tap has been missing for
//! [`STALE_AFTER`]: a booting instance claims its slot before its tap exists.

use super::host_net::{InstanceNet, SLOT_COUNT, preferred_slot, tap_name_for_slot};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use tracing::warn;

/// How long a claim may go without its tap before another instance takes the
/// slot over: longer than any boot.
const STALE_AFTER: Duration = Duration::from_secs(600);

/// The allocator of this server, under its config directory.
pub(crate) fn slots() -> &'static NetSlots {
    static SLOTS: OnceLock<NetSlots> = OnceLock::new();
    SLOTS.get_or_init(|| {
        NetSlots::new(PathBuf::from(crate::config::config::get_config_dir()).join("net-slots"))
    })
}

pub(crate) struct NetSlots {
    dir: PathBuf,
    tap_exists: fn(&str) -> bool,
}

impl NetSlots {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            tap_exists: super::tap::TapDevice::exists,
        }
    }

    fn slots_dir(&self) -> PathBuf {
        self.dir.join("slots")
    }

    fn instances_dir(&self) -> PathBuf {
        self.dir.join("instances")
    }

    fn slot_path(&self, slot: u32) -> PathBuf {
        self.slots_dir().join(slot.to_string())
    }

    fn instance_path(&self, instance_id: &str) -> PathBuf {
        self.instances_dir().join(instance_id)
    }

    /// The slot `instance_id` holds a claim on, if any.
    fn claimed_slot(&self, instance_id: &str) -> Option<u32> {
        std::fs::read_to_string(self.instance_path(instance_id))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// The network of `instance_id`: its claimed slot, or the slot its id
    /// hashes to when it holds no claim.
    pub(crate) fn lookup(&self, instance_id: &str) -> InstanceNet {
        let slot = self
            .claimed_slot(instance_id)
            .unwrap_or_else(|| preferred_slot(instance_id));
        InstanceNet::for_slot(slot, instance_id)
    }

    /// Claim a free slot for `instance_id` and return its network. Claiming
    /// again returns the slot it already holds.
    pub(crate) fn claim(&self, instance_id: &str) -> std::io::Result<InstanceNet> {
        if let Some(slot) = self.claimed_slot(instance_id) {
            return Ok(InstanceNet::for_slot(slot, instance_id));
        }
        std::fs::create_dir_all(self.slots_dir())?;
        std::fs::create_dir_all(self.instances_dir())?;

        let start = preferred_slot(instance_id);
        for offset in 0..SLOT_COUNT {
            let slot = (start + offset) % SLOT_COUNT;
            if self.try_claim(slot, instance_id)? {
                std::fs::write(self.instance_path(instance_id), slot.to_string())?;
                return Ok(InstanceNet::for_slot(slot, instance_id));
            }
        }
        Err(std::io::Error::other(format!(
            "no free network slot left: all {SLOT_COUNT} guest subnets are in use"
        )))
    }

    /// Claim `slot` for `instance_id` if it is free. A stale claim is removed
    /// first.
    fn try_claim(&self, slot: u32, instance_id: &str) -> std::io::Result<bool> {
        if (self.tap_exists)(&tap_name_for_slot(slot)) {
            return Ok(false);
        }
        let path = self.slot_path(slot);
        if path.exists() {
            if !self.is_stale(&path) {
                return Ok(false);
            }
            let _ = std::fs::remove_file(&path);
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(instance_id.as_bytes())?;
                Ok(true)
            }
            // Another instance claimed it in between.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// A claim whose tap is missing (checked by the caller) and that is older
    /// than [`STALE_AFTER`]: left by a boot that failed.
    fn is_stale(&self, path: &Path) -> bool {
        let age = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        matches!(age, Some(age) if age >= STALE_AFTER)
    }

    /// Release the slot `instance_id` holds, if any.
    pub(crate) fn release(&self, instance_id: &str) {
        let Some(slot) = self.claimed_slot(instance_id) else {
            return;
        };
        let path = self.slot_path(slot);
        // Only the claim of this instance: a stale one may have been taken over.
        if std::fs::read_to_string(&path).is_ok_and(|owner| owner == instance_id)
            && let Err(e) = std::fs::remove_file(&path)
        {
            warn!("could not release network slot {slot} of {instance_id}: {e}");
        }
        let _ = std::fs::remove_file(self.instance_path(instance_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocator(label: &str) -> (NetSlots, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ring-net-slots-{}-{}", label, uuid::Uuid::new_v4()));
        let slots = NetSlots {
            dir: dir.clone(),
            tap_exists: |_| false,
        };
        (slots, dir)
    }

    /// Two instance ids that hash to the same slot.
    fn colliding_ids() -> (String, String) {
        let mut seen = std::collections::HashMap::new();
        for i in 0.. {
            let id = format!("dep-{i}");
            if let Some(other) = seen.insert(preferred_slot(&id), id.clone()) {
                return (other, id);
            }
        }
        unreachable!()
    }

    #[test]
    fn instances_hashing_to_one_slot_get_distinct_networks() {
        let (slots, dir) = allocator("collide");
        let (a, b) = colliding_ids();
        assert_eq!(
            InstanceNet::for_instance(&a).tap_name,
            InstanceNet::for_instance(&b).tap_name
        );

        let net_a = slots.claim(&a).unwrap();
        let net_b = slots.claim(&b).unwrap();

        assert_ne!(net_a.tap_name, net_b.tap_name);
        assert_ne!(net_a.guest_ip, net_b.guest_ip);
        assert_ne!(net_a.host_ip, net_b.host_ip);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_first_claim_takes_the_hashed_slot() {
        let (slots, dir) = allocator("first");
        let net = slots.claim("dep-1").unwrap();
        assert_eq!(net.tap_name, InstanceNet::for_instance("dep-1").tap_name);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_claim_survives_a_new_allocator_on_the_same_directory() {
        let (slots, dir) = allocator("restart");
        let (a, b) = colliding_ids();
        slots.claim(&a).unwrap();
        let claimed = slots.claim(&b).unwrap();

        let restarted = NetSlots {
            dir: dir.clone(),
            tap_exists: |_| false,
        };
        assert_eq!(restarted.lookup(&b).tap_name, claimed.tap_name);
        assert_eq!(restarted.claim(&b).unwrap().tap_name, claimed.tap_name);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_instance_without_a_claim_keeps_its_hashed_slot() {
        let (slots, dir) = allocator("legacy");
        assert_eq!(
            slots.lookup("booted-before-the-upgrade").tap_name,
            InstanceNet::for_instance("booted-before-the-upgrade").tap_name
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_slot_whose_tap_exists_is_skipped() {
        let dir = std::env::temp_dir().join(format!("ring-net-slots-tap-{}", uuid::Uuid::new_v4()));
        let taken = tap_name_for_slot(preferred_slot("dep-1"));
        static TAKEN: OnceLock<String> = OnceLock::new();
        TAKEN.set(taken).unwrap();
        let slots = NetSlots {
            dir: dir.clone(),
            tap_exists: |name| Some(name) == TAKEN.get().map(String::as_str),
        };

        let net = slots.claim("dep-1").unwrap();

        assert_ne!(&net.tap_name, TAKEN.get().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_released_slot_can_be_claimed_again() {
        let (slots, dir) = allocator("release");
        let (a, b) = colliding_ids();
        let net_a = slots.claim(&a).unwrap();
        slots.release(&a);

        assert_eq!(slots.claim(&b).unwrap().tap_name, net_a.tap_name);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_fresh_claim_without_its_tap_is_not_taken_over() {
        let (slots, dir) = allocator("fresh");
        let (a, b) = colliding_ids();
        let net_a = slots.claim(&a).unwrap();
        // The tap of `a` does not exist yet (it is still booting).
        assert_ne!(slots.claim(&b).unwrap().tap_name, net_a.tap_name);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stale_claim_is_taken_over() {
        let (slots, dir) = allocator("stale");
        let (a, b) = colliding_ids();
        let net_a = slots.claim(&a).unwrap();
        let path = slots.slot_path(preferred_slot(&a));
        let old = SystemTime::now() - STALE_AFTER - Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        assert_eq!(slots.claim(&b).unwrap().tap_name, net_a.tap_name);
        // `a` lost its claim: releasing it leaves `b`'s alone.
        slots.release(&a);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), b);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
