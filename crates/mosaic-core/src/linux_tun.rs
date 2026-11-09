//! Linux: Mosaic's own TUN stack, prepared by a small privileged helper
//! (`crates/mosaic-tun-helper`, installed next to Mosaic; run from a copy that
//! carries `cap_net_admin`, granted once like the sing-box adapter). The helper:
//!   1. creates a *persistent* TUN interface owned by the user, so the unprivileged
//!      Mosaic process can attach to it (TUNSETIFF on an interface you own needs no rights);
//!   2. after Mosaic has attached ("go"), sets the routes: 0/1 + 128/1 into the TUN,
//!      /32 bypass routes for node servers via the physical gateway, the system's DNS
//!      servers into the TUN, and the interface DNS in systemd-resolved;
//!   3. waits until its stdin closes (Mosaic disconnected, quit or crashed) and removes
//!      everything again.
//! Mosaic's own direct sockets are bound to the physical interface (SO_BINDTODEVICE),
//! so they never loop back into the tunnel.
//!
//! Protocol on stdout: `created`, then `ready`, or `error: …` at any point.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

pub const TUN_NAME: &str = "Mosaic";
pub const HELPER_ARG: &str = "run";

// ---------------------------------------------------------------- shared helpers (both sides)

/// The physical default route: (interface, gateway) from /proc/net/route, lowest metric.
pub fn physical_default_route() -> Option<(String, Option<Ipv4Addr>)> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    let mut best: Option<(u32, String, Option<Ipv4Addr>)> = None;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 || f[0] == TUN_NAME || f[1] != "00000000" || f[7] != "00000000" {
            continue;
        }
        let gw = u32::from_str_radix(f[2], 16).ok().map(|g| Ipv4Addr::from(g.to_le_bytes())).filter(|g| !g.is_unspecified());
        let metric: u32 = f[6].parse().unwrap_or(u32::MAX);
        if best.as_ref().map_or(true, |b| metric < b.0) {
            best = Some((metric, f[0].to_string(), gw));
        }
    }
    best.map(|(_, i, g)| (i, g))
}

pub fn ifindex(name: &str) -> Option<u32> {
    std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex")).ok()?.trim().parse().ok()
}

/// IPv4 nameservers from /etc/resolv.conf that are not local stubs (127.x).
pub fn resolv_nameservers() -> Vec<Ipv4Addr> {
    let text = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|v| v.trim().parse::<Ipv4Addr>().ok())
        .filter(|ip| !ip.is_loopback())
        .collect()
}

/// Our copy of the TUN helper (installed next to the Mosaic binary) that carries the capability.
pub fn helper_binary(dir: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let here = exe.parent().ok_or("no exe dir")?;
    // deb/rpm: /usr/bin, AppImage: $APPDIR/usr/bin, development: target/<profile>.
    let src = [here.join("mosaic-tun-helper"), here.join("../lib/Mosaic/mosaic-tun-helper")]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or("mosaic-tun-helper was not found next to Mosaic (use the sing-box TUN stack, or reinstall)")?;
    crate::linux::copy_if_changed(&src, &dir.join("mosaic-tun-helper"))
}

const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
const IFF_TUN: libc::c_short = 0x0001;
const IFF_NO_PI: libc::c_short = 0x1000;

#[repr(C)]
pub struct IfReq {
    name: [u8; 16],
    flags: libc::c_short,
    _pad: [u8; 22],
}

pub fn ifreq(name: &str) -> IfReq {
    let mut r = IfReq { name: [0; 16], flags: IFF_TUN | IFF_NO_PI, _pad: [0; 22] };
    r.name[..name.len().min(15)].copy_from_slice(&name.as_bytes()[..name.len().min(15)]);
    r
}

/// Open /dev/net/tun and attach to `name` (creates it if we have the right to).
pub fn open_tun(name: &str) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let mut req = ifreq(name);
    if unsafe { libc::ioctl(fd, TUNSETIFF as _, &mut req as *mut IfReq) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(owned)
}


