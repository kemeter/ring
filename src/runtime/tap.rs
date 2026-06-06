//! Host TAP device management for VM runtimes that don't create their own.
//!
//! Cloud Hypervisor creates the tap itself (it holds `CAP_NET_ADMIN` and is
//! handed a tap *name* to bring up). Firecracker does **not**: it expects the
//! tap device to already exist on the host and only references it by name. So
//! for Firecracker, Ring owns the tap's whole lifecycle — create it, give the
//! host side an IP, bring it up before boot, and delete it on teardown.
//!
//! This is done with **direct syscalls** (`ioctl`), not by shelling out to
//! `ip`. Shelling out is tempting but broken under capabilities: `ring-server`
//! can hold `CAP_NET_ADMIN`, but a forked `ip` process does **not** inherit it
//! (the capability is in `ring`'s permitted/effective sets, not the *ambient*
//! set), so `ip tuntap add` fails with `EPERM`. Doing the ioctls in-process
//! keeps the capability where it's needed.
//!
//! Requires `ring-server` to run with `CAP_NET_ADMIN` (or as root). When the
//! capability is missing the syscalls return `EPERM`, which we surface as an
//! actionable [`RuntimeError::NetworkCreationFailed`] telling the operator how
//! to grant it.
//!
//! The parameters (tap name, host IP, prefix) come from
//! [`crate::runtime::host_net::InstanceNet`], so allocation stays a pure
//! function of the instance id and two replicas never collide.

use crate::runtime::error::RuntimeError;
use std::fs::OpenOptions;
use std::io;
use std::mem;
use std::os::fd::AsRawFd;

// Linux ABI constants (stable across kernels). Defined here rather than pulled
// from libc because not all are exported on every libc version.
const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const TUNSETPERSIST: libc::c_ulong = 0x4004_54cb;
const IFF_TAP: libc::c_short = 0x0002;
const IFF_NO_PI: libc::c_short = 0x1000;

const SIOCSIFADDR: libc::c_ulong = 0x8916;
const SIOCSIFNETMASK: libc::c_ulong = 0x891c;
const SIOCSIFFLAGS: libc::c_ulong = 0x8914;
const SIOCGIFFLAGS: libc::c_ulong = 0x8913;
const IFF_UP: libc::c_short = 0x1;
const IFF_RUNNING: libc::c_short = 0x40;

/// A live host tap device. Created via [`TapDevice::create`]; deleting it
/// (via [`TapDevice::delete`] or `Drop`) removes the persistent interface.
///
/// Crucially, no `/dev/net/tun` fd is kept open after creation. The device is
/// made *persistent*, so it survives the creating fd closing — and the fd MUST
/// be closed, otherwise it stays attached as the tap's backend and Firecracker
/// gets `EBUSY` ("Resource busy") when it tries to open the same tap. We only
/// reopen briefly at `delete` time to clear persistence.
pub(crate) struct TapDevice {
    name: String,
    live: bool,
}

impl TapDevice {
    /// Create a tap device named `name`, assign `host_ip/prefix_len` to its host
    /// side, and bring it up. The device is made persistent and our fd is closed
    /// before returning, so Firecracker can open it as its backend.
    pub(crate) fn create(name: &str, host_ip: &str, prefix_len: u8) -> Result<Self, RuntimeError> {
        if name.len() >= libc::IFNAMSIZ {
            return Err(RuntimeError::NetworkCreationFailed(format!(
                "tap name '{}' exceeds IFNAMSIZ-1 ({})",
                name,
                libc::IFNAMSIZ - 1
            )));
        }

        // 1. Open the clone device and create the tap via TUNSETIFF, then mark
        //    it persistent so it survives the fd closing. The fd is dropped at
        //    the end of this block — leaving it open would keep the tap's
        //    backend attached and make Firecracker's open fail with EBUSY.
        {
            let tun = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/net/tun")
                .map_err(|e| Self::map_err(name, "open /dev/net/tun", e))?;

            let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
            write_ifname(&mut ifr, name);
            ifr.ifr_ifru.ifru_flags = IFF_TAP | IFF_NO_PI;
            unsafe {
                ioctl(
                    tun.as_raw_fd(),
                    TUNSETIFF,
                    &mut ifr as *mut _ as *mut libc::c_void,
                )
            }
            .map_err(|e| Self::map_err(name, "TUNSETIFF (create tap)", e))?;

            unsafe { ioctl_int(tun.as_raw_fd(), TUNSETPERSIST, 1) }
                .map_err(|e| Self::map_err(name, "TUNSETPERSIST", e))?;
            // `tun` drops here → fd closed, backend detached, device persists.
        }

        let dev = Self {
            name: name.to_string(),
            live: true,
        };

        // 2. Configure the host-side IP, netmask, and bring the link up via an
        //    AF_INET socket. On any failure, roll the device back.
        if let Err(e) = dev.configure(host_ip, prefix_len) {
            let mut dev = dev;
            dev.delete();
            return Err(e);
        }

        Ok(dev)
    }

    fn configure(&self, host_ip: &str, prefix_len: u8) -> Result<(), RuntimeError> {
        let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if sock < 0 {
            return Err(Self::map_err(
                &self.name,
                "socket(AF_INET)",
                io::Error::last_os_error(),
            ));
        }
        // Ensure the socket fd is closed however we leave this function.
        let _guard = FdGuard(sock);

        let ip = parse_ipv4(host_ip).ok_or_else(|| {
            RuntimeError::NetworkCreationFailed(format!("invalid host IP '{}'", host_ip))
        })?;
        let mask = prefix_to_mask(prefix_len);

        // SIOCSIFADDR — set the interface address.
        let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
        write_ifname(&mut ifr, &self.name);
        write_sockaddr_in(&mut ifr, ip);
        unsafe { ioctl(sock, SIOCSIFADDR, &mut ifr as *mut _ as *mut libc::c_void) }
            .map_err(|e| Self::map_err(&self.name, "SIOCSIFADDR", e))?;

        // SIOCSIFNETMASK — set the netmask.
        let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
        write_ifname(&mut ifr, &self.name);
        write_sockaddr_in(&mut ifr, mask);
        unsafe {
            ioctl(
                sock,
                SIOCSIFNETMASK,
                &mut ifr as *mut _ as *mut libc::c_void,
            )
        }
        .map_err(|e| Self::map_err(&self.name, "SIOCSIFNETMASK", e))?;

        // SIOCGIFFLAGS + SIOCSIFFLAGS — bring the link up.
        let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
        write_ifname(&mut ifr, &self.name);
        unsafe { ioctl(sock, SIOCGIFFLAGS, &mut ifr as *mut _ as *mut libc::c_void) }
            .map_err(|e| Self::map_err(&self.name, "SIOCGIFFLAGS", e))?;
        unsafe {
            ifr.ifr_ifru.ifru_flags |= IFF_UP | IFF_RUNNING;
        }
        unsafe { ioctl(sock, SIOCSIFFLAGS, &mut ifr as *mut _ as *mut libc::c_void) }
            .map_err(|e| Self::map_err(&self.name, "SIOCSIFFLAGS (up)", e))?;

        Ok(())
    }

    /// Delete the device from the host. We hold no fd, so reopen `/dev/net/tun`,
    /// re-attach to the existing tap by name, and clear persistence — once
    /// persistence is off and this transient fd closes, the kernel removes the
    /// interface.
    ///
    /// Re-attach can briefly fail with `EBUSY` if the VM process hasn't fully
    /// released the tap yet (the caller kills it first, but the fd close is
    /// asynchronous), so retry a few times before giving up. Idempotent.
    pub(crate) fn delete(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;

        for _ in 0..20 {
            let Ok(tun) = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/net/tun")
            else {
                return;
            };
            let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
            write_ifname(&mut ifr, &self.name);
            ifr.ifr_ifru.ifru_flags = IFF_TAP | IFF_NO_PI;
            // Re-attach to the existing tap, then turn persistence off so the
            // kernel removes it when this fd closes.
            if unsafe {
                ioctl(
                    tun.as_raw_fd(),
                    TUNSETIFF,
                    &mut ifr as *mut _ as *mut libc::c_void,
                )
            }
            .is_ok()
            {
                let _ = unsafe { ioctl_int(tun.as_raw_fd(), TUNSETPERSIST, 0) };
                return;
            }
            // Busy — the previous holder hasn't let go yet. Brief spin-wait.
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Translate an ioctl/syscall error into an actionable runtime error,
    /// special-casing `EPERM` (the missing-capability case).
    fn map_err(name: &str, op: &str, e: io::Error) -> RuntimeError {
        if e.raw_os_error() == Some(libc::EPERM) {
            return RuntimeError::NetworkCreationFailed(format!(
                "could not create tap '{}': operation not permitted ({}). Run ring-server with \
                 CAP_NET_ADMIN (e.g. `setcap cap_net_admin+ep $(command -v ring)`) or as root",
                name, op
            ));
        }
        RuntimeError::NetworkCreationFailed(format!("tap '{}' {} failed: {}", name, op, e))
    }
}

impl Drop for TapDevice {
    fn drop(&mut self) {
        self.delete();
    }
}

/// Closes a raw fd on drop.
struct FdGuard(libc::c_int);
impl Drop for FdGuard {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Thin wrapper over `libc::ioctl` (pointer arg) returning `io::Result`.
unsafe fn ioctl(fd: libc::c_int, request: libc::c_ulong, arg: *mut libc::c_void) -> io::Result<()> {
    let rc = unsafe { libc::ioctl(fd, request as _, arg) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `libc::ioctl` variant for requests that take an integer by value (e.g.
/// `TUNSETPERSIST`), rather than a pointer to a struct.
unsafe fn ioctl_int(fd: libc::c_int, request: libc::c_ulong, arg: libc::c_int) -> io::Result<()> {
    let rc = unsafe { libc::ioctl(fd, request as _, arg) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Copy an interface name into the `ifr_name` field (NUL-padded).
fn write_ifname(ifr: &mut libc::ifreq, name: &str) {
    let bytes = name.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        ifr.ifr_name[i] = *b as libc::c_char;
    }
}

/// Write an IPv4 address into the `ifr_addr` field as a `sockaddr_in`.
fn write_sockaddr_in(ifr: &mut libc::ifreq, addr_be: u32) {
    let sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr { s_addr: addr_be },
        sin_zero: [0; 8],
    };
    unsafe {
        let dst = &mut ifr.ifr_ifru.ifru_addr as *mut libc::sockaddr as *mut libc::sockaddr_in;
        *dst = sin;
    }
}

/// Parse a dotted IPv4 into a big-endian (network-order) u32 suitable for
/// `s_addr`.
fn parse_ipv4(s: &str) -> Option<u32> {
    let addr: std::net::Ipv4Addr = s.parse().ok()?;
    // `s_addr` is in network byte order; Ipv4Addr::octets() is already MSB-first.
    Some(u32::from_ne_bytes(addr.octets()))
}

/// Build the network-order netmask u32 for a CIDR prefix length.
fn prefix_to_mask(prefix_len: u8) -> u32 {
    let bits: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len as u32)
    };
    // `bits` is host-order MSB-first; convert to network order bytes.
    u32::from_ne_bytes(bits.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_30_mask_is_252() {
        // /30 → 255.255.255.252. Last octet 252 in network order.
        let mask = prefix_to_mask(30);
        let octets = mask.to_ne_bytes();
        assert_eq!(octets, [255, 255, 255, 252]);
    }

    #[test]
    fn parse_ipv4_roundtrip() {
        let be = parse_ipv4("10.42.1.1").unwrap();
        assert_eq!(be.to_ne_bytes(), [10, 42, 1, 1]);
    }

    #[test]
    fn parse_ipv4_rejects_garbage() {
        assert!(parse_ipv4("not-an-ip").is_none());
    }
}
