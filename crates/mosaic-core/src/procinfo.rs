//! Which program owns a local socket — for per-application rules.
//!
//! Windows: GetExtendedTcpTable / GetExtendedUdpTable give the owning PID of
//! a local address:port, QueryFullProcessImageNameW gives its executable.
//! Results are cached briefly; lookups only happen when a rule needs them.
//!
//! Android: the app layer answers through a hook (ConnectivityManager.
//! getConnectionOwnerUid → package name), see `set_owner_hook`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// (protocol, local/app side, remote side) → owner name.
pub type OwnerHook = Box<dyn Fn(Proto, SocketAddr, SocketAddr) -> Option<String> + Send + Sync>;
static OWNER_HOOK: OnceLock<OwnerHook> = OnceLock::new();

/// Install the platform's owner lookup (Android: package names). Set once.
pub fn set_owner_hook(f: OwnerHook) {
    let _ = OWNER_HOOK.set(f);
}

/// Owner of a flow captured by the TUN: `local` is the application side, `remote`
/// its destination. Lower-case ("telegram.exe", "org.telegram.messenger").
pub fn flow_owner(local: SocketAddr, remote: SocketAddr, proto: Proto) -> Option<String> {
    if let Some(hook) = OWNER_HOOK.get() {
        return hook(proto, local, remote).map(|s| s.to_ascii_lowercase());
    }
    process_of(local, proto)
}

static NAMES: Mutex<Option<HashMap<u32, (String, Instant)>>> = Mutex::new(None);
const NAME_TTL: Duration = Duration::from_secs(30);

/// Lower-case executable file name ("telegram.exe") of the process that owns
/// the local endpoint `local` (the application side of a connection).
pub fn process_of(local: SocketAddr, proto: Proto) -> Option<String> {
    let pid = owner_pid(local, proto)?;
    if pid == 0 || pid == 4 {
        return None; // idle / System
    }
    {
        let g = NAMES.lock().unwrap();
        if let Some((n, at)) = g.as_ref().and_then(|m| m.get(&pid)) {
            if at.elapsed() < NAME_TTL {
                return Some(n.clone());
            }
        }
    }
    let name = image_name(pid)?;
    let mut g = NAMES.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() > 1024 {
        m.clear();
    }
    m.insert(pid, (name.clone(), Instant::now()));
    Some(name)
}

/// "C:\\Apps\\Telegram.exe" → "telegram.exe"
pub fn file_name_lower(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase()
}

/// Ports in the IP helper tables are stored in network byte order in the low 16 bits.
pub fn table_port(v: u32) -> u16 {
    u16::from_be((v & 0xffff) as u16)
}

#[cfg(windows)]
fn owner_pid(local: SocketAddr, proto: Proto) -> Option<u32> {
    win::owner_pid(local, proto)
}

#[cfg(target_os = "linux")]
fn owner_pid(local: SocketAddr, proto: Proto) -> Option<u32> {
    lin::owner_pid(local, proto)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn owner_pid(_local: SocketAddr, _proto: Proto) -> Option<u32> {
    None
}

#[cfg(windows)]
fn image_name(pid: u32) -> Option<String> {
    win::image_name(pid)
}

#[cfg(target_os = "linux")]
fn image_name(pid: u32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    Some(file_name_lower(&exe.to_string_lossy()))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn image_name(_pid: u32) -> Option<String> {
    None
}

/// Linux: /proc/net/{tcp,udp}[6] give the socket inode of a local port,
/// /proc/<pid>/fd links ("socket:[inode]") give its process.
#[cfg(target_os = "linux")]
mod lin {
    use super::Proto;
    use std::net::SocketAddr;

    /// Socket inode for a local port in one /proc/net table (text).
    pub(super) fn inode_in(table: &str, port: u16) -> Option<u64> {
        for line in table.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let local_port = f[1].rsplit(':').next().and_then(|p| u16::from_str_radix(p, 16).ok());
            if local_port != Some(port) || f[3] == "0A" {
                continue; // other port, or a listening socket
            }
            if let Ok(inode) = f[9].parse::<u64>() {
                if inode != 0 {
                    return Some(inode);
                }
            }
        }
        None
    }

    pub(super) fn owner_pid(local: SocketAddr, proto: Proto) -> Option<u32> {
        let names: &[&str] = match proto {
            Proto::Tcp => &["/proc/net/tcp", "/proc/net/tcp6"],
            Proto::Udp => &["/proc/net/udp", "/proc/net/udp6"],
        };
        let inode = names
            .iter()
            .filter_map(|n| std::fs::read_to_string(n).ok())
            .find_map(|t| inode_in(&t, local.port()))?;
        let want = format!("socket:[{inode}]");
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
            for fd in fds.flatten() {
                if std::fs::read_link(fd.path()).map(|l| l.as_os_str() == want.as_str()).unwrap_or(false) {
                    return Some(pid);
                }
            }
        }
        None
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    const TCP_TABLE_OWNER_PID_ALL: u32 = 5;
    const UDP_TABLE_OWNER_PID: u32 = 1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetExtendedTcpTable(table: *mut c_void, size: *mut u32, order: i32, af: u32, class: u32, reserved: u32) -> u32;
        fn GetExtendedUdpTable(table: *mut c_void, size: *mut u32, order: i32, af: u32, class: u32, reserved: u32) -> u32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn QueryFullProcessImageNameW(process: isize, flags: u32, name: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }

    /// Fetch a table into a u32-aligned buffer (the rows are DWORD-based).
    fn fetch(proto: Proto, af: u32) -> Option<Vec<u32>> {
        let mut size: u32 = 0;
        let call = |buf: *mut c_void, size: &mut u32| unsafe {
            match proto {
                Proto::Tcp => GetExtendedTcpTable(buf, size, 0, af, TCP_TABLE_OWNER_PID_ALL, 0),
                Proto::Udp => GetExtendedUdpTable(buf, size, 0, af, UDP_TABLE_OWNER_PID, 0),
            }
        };
        let rc = call(std::ptr::null_mut(), &mut size);
        if rc != ERROR_INSUFFICIENT_BUFFER && rc != 0 {
            return None;
        }
        for _ in 0..3 {
            let mut buf = vec![0u32; (size as usize + 3) / 4 + 64];
            size = (buf.len() * 4) as u32;
            match call(buf.as_mut_ptr() as *mut c_void, &mut size) {
                0 => return Some(buf),
                ERROR_INSUFFICIENT_BUFFER => continue,
                _ => return None,
            }
        }
        None
    }

    fn v6(words: &[u32]) -> Ipv6Addr {
        let mut b = [0u8; 16];
        for (i, w) in words.iter().take(4).enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
        }
        Ipv6Addr::from(b)
    }

    pub fn owner_pid(local: SocketAddr, proto: Proto) -> Option<u32> {
        let want_port = local.port();
        let (af, ip) = match local.ip() {
            IpAddr::V4(a) => (AF_INET, IpAddr::V4(a)),
            IpAddr::V6(a) => match a.to_ipv4_mapped() {
                Some(v4) => (AF_INET, IpAddr::V4(v4)),
                None => (AF_INET6, IpAddr::V6(a)),
            },
        };
        let t = fetch(proto, af)?;
        let n = *t.first()? as usize;
        // Row layouts in DWORDs:
        //   TCP v4: state, localAddr, localPort, remoteAddr, remotePort, pid      (6)
        //   UDP v4: localAddr, localPort, pid                                     (3)
        //   TCP v6: localAddr[4], localScope, localPort, remoteAddr[4], remoteScope, remotePort, state, pid (14)
        //   UDP v6: localAddr[4], localScope, localPort, pid                      (7)
        let (stride, addr_at, port_at, pid_at, is_v6) = match (proto, af == AF_INET6) {
            (Proto::Tcp, false) => (6, 1, 2, 5, false),
            (Proto::Udp, false) => (3, 0, 1, 2, false),
            (Proto::Tcp, true) => (14, 0, 5, 13, true),
            (Proto::Udp, true) => (7, 0, 5, 6, true),
        };
        let mut port_only = None;
        for i in 0..n {
            let row = t.get(1 + i * stride..1 + (i + 1) * stride)?;
            if super::table_port(row[port_at]) != want_port {
                continue;
            }
            let row_ip = if is_v6 {
                IpAddr::V6(v6(&row[addr_at..addr_at + 4]))
            } else {
                IpAddr::V4(Ipv4Addr::from(row[addr_at].to_ne_bytes()))
            };
            if row_ip == ip {
                return Some(row[pid_at]);
            }
            // Wildcard-bound sockets (0.0.0.0) — common for UDP.
            if row_ip.is_unspecified() {
                port_only = Some(row[pid_at]);
            }
        }
        port_only
    }

    pub fn image_name(pid: u32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return None;
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                return None;
            }
            Some(super::file_name_lower(&String::from_utf16_lossy(&buf[..len as usize])))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_net_parse() {
        let t = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1ED2 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11111 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:D431 0100007F:1ED2 01 00000000:00000000 00:00000000 00000000  1000        0 22222 1 0000000000000000 20 4 30 10 -1\n";
        assert_eq!(lin::inode_in(t, 0xD431), Some(22222));
        assert_eq!(lin::inode_in(t, 0x1ED2), None); // listening
    }

    #[test]
    fn helpers() {
        assert_eq!(file_name_lower(r"C:\Program Files\Telegram\Telegram.exe"), "telegram.exe");
        assert_eq!(file_name_lower("/usr/bin/curl"), "curl");
        // port 443 = 0x01BB stored as bytes BB 01 in the low word on little-endian
        assert_eq!(table_port(u32::from_ne_bytes([0x01, 0xbb, 0, 0])), 443);
    }
}
