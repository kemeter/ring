//! `ring-init`: PID 1 of the initramfs Ring boots a Firecracker microVM with
//! when its image is a squashfs.
//!
//! The squashfs is shared, read-only, by every microVM of the deployment, and
//! each microVM gets its own small writable ext4 disk. This program stacks the
//! two into one root filesystem with overlayfs, then hands over to the image's
//! own init:
//!
//! 1. mount `/proc`, `/sys` and `/dev` (devtmpfs, which creates the disk nodes);
//! 2. mount the squashfs read-only on `/lower` and the writable disk on `/upper`;
//! 3. mount an overlay of the two on `/newroot`;
//! 4. move `/proc`, `/sys` and `/dev` into it, make it the root, and exec the
//!    image's init.
//!
//! The devices and the init come from the kernel command line, with defaults
//! matching the drive order Ring attaches:
//!
//! - `ring.lower=` the squashfs device (default `/dev/vda`)
//! - `ring.upper=` the writable disk (default `/dev/vdb`)
//! - `ring.init=` the init to run in the new root (default `/sbin/init`)
//!
//! On any failure it prints the reason on the console and exits: PID 1 exiting
//! panics the kernel, which with `panic=1 reboot=k` stops the microVM, so Ring
//! sees the instance fail and can read why in its console log.

use std::ffi::CString;
use std::path::Path;
use std::time::{Duration, Instant};

use nix::mount::{MsFlags, mount};
use nix::unistd::{chdir, chroot, execv};

/// How long to wait for a disk node to appear after devtmpfs is mounted.
const DEVICE_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
struct BootConfig {
    lower: String,
    upper: String,
    init: String,
}

/// Read Ring's settings from the kernel command line, falling back to the
/// drive order Ring uses.
fn parse_cmdline(cmdline: &str) -> BootConfig {
    let mut config = BootConfig {
        lower: "/dev/vda".to_string(),
        upper: "/dev/vdb".to_string(),
        init: "/sbin/init".to_string(),
    };
    for arg in cmdline.split_whitespace() {
        if let Some(value) = arg.strip_prefix("ring.lower=") {
            config.lower = value.to_string();
        } else if let Some(value) = arg.strip_prefix("ring.upper=") {
            config.upper = value.to_string();
        } else if let Some(value) = arg.strip_prefix("ring.init=") {
            config.init = value.to_string();
        }
    }
    config
}

fn main() {
    if let Err(message) = run() {
        eprintln!("ring-init: {}", message);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    mount_fs("proc", "/proc", "proc", MsFlags::empty(), None)?;
    mount_fs("sysfs", "/sys", "sysfs", MsFlags::empty(), None)?;
    mount_fs("devtmpfs", "/dev", "devtmpfs", MsFlags::empty(), None)?;

    let cmdline = std::fs::read_to_string("/proc/cmdline")
        .map_err(|e| format!("cannot read /proc/cmdline: {}", e))?;
    let config = parse_cmdline(&cmdline);

    wait_for(&config.lower)?;
    wait_for(&config.upper)?;

    mount_fs(
        &config.lower,
        "/lower",
        "squashfs",
        MsFlags::MS_RDONLY,
        None,
    )?;
    mount_fs(&config.upper, "/upper", "ext4", MsFlags::empty(), None)?;
    for dir in ["/upper/data", "/upper/work"] {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir, e))?;
    }
    mount_fs(
        "overlay",
        "/newroot",
        "overlay",
        MsFlags::empty(),
        Some("lowerdir=/lower,upperdir=/upper/data,workdir=/upper/work"),
    )?;

    // Hand the kernel filesystems over to the new root rather than mounting
    // them again: its init finds them already in place.
    for dir in ["/proc", "/sys", "/dev"] {
        let target = format!("/newroot{}", dir);
        std::fs::create_dir_all(&target).map_err(|e| format!("cannot create {}: {}", target, e))?;
        mount(
            Some(dir),
            target.as_str(),
            None::<&str>,
            MsFlags::MS_MOVE,
            None::<&str>,
        )
        .map_err(|e| format!("cannot move {} into the new root: {}", dir, e))?;
    }

    // The initramfs root cannot be unmounted; moving the new root over it and
    // chrooting is how switch_root does it.
    chdir("/newroot").map_err(|e| format!("cannot enter /newroot: {}", e))?;
    mount(Some("."), "/", None::<&str>, MsFlags::MS_MOVE, None::<&str>)
        .map_err(|e| format!("cannot move the new root to /: {}", e))?;
    chroot(".").map_err(|e| format!("cannot chroot into the new root: {}", e))?;
    chdir("/").map_err(|e| format!("cannot enter the new root: {}", e))?;

    let init = CString::new(config.init.clone())
        .map_err(|_| format!("invalid init path {:?}", config.init))?;
    execv(&init, &[&init]).map_err(|e| format!("cannot run {}: {}", config.init, e))?;
    Ok(())
}

fn mount_fs(
    source: &str,
    target: &str,
    fstype: &str,
    flags: MsFlags,
    data: Option<&str>,
) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|e| format!("cannot create {}: {}", target, e))?;
    mount(Some(source), target, Some(fstype), flags, data)
        .map_err(|e| format!("cannot mount {} ({}) on {}: {}", source, fstype, target, e))
}

/// Wait for a device node: devtmpfs creates it as soon as the kernel has
/// probed the disk, which is normally already done when PID 1 starts.
fn wait_for(device: &str) -> Result<(), String> {
    let deadline = Instant::now() + DEVICE_WAIT;
    while !Path::new(device).exists() {
        if Instant::now() >= deadline {
            return Err(format!("{} did not appear", device));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_drive_order_ring_attaches() {
        let config = parse_cmdline("console=ttyS0 reboot=k panic=1 pci=off");
        assert_eq!(
            config,
            BootConfig {
                lower: "/dev/vda".to_string(),
                upper: "/dev/vdb".to_string(),
                init: "/sbin/init".to_string(),
            }
        );
    }

    #[test]
    fn ring_arguments_override_the_defaults() {
        let config = parse_cmdline(
            "console=ttyS0 ring.lower=/dev/vdc ring.upper=/dev/vdd ring.init=/lib/systemd/systemd",
        );
        assert_eq!(config.lower, "/dev/vdc");
        assert_eq!(config.upper, "/dev/vdd");
        assert_eq!(config.init, "/lib/systemd/systemd");
    }
}
