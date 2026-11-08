//! Outbound sockets for "direct" traffic. While the native TUN is up, the
//! default route points into the tunnel, so the hub's own sockets are pinned
//! to the physical interface (IP_UNICAST_IF on Windows) to avoid a loop.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::net::{TcpSocket, TcpStream, UdpSocket};

use crate::resolver::Resolver;

/// Interface index to pin direct sockets to; 0 = no pinning.
static BYPASS_IFINDEX: AtomicU32 = AtomicU32::new(0);

pub fn set_bypass_interface(ifindex: u32) {
    BYPASS_IFINDEX.store(ifindex, Ordering::SeqCst);
}

pub fn bypass_interface() -> u32 {
    BYPASS_IFINDEX.load(Ordering::SeqCst)
}

#[cfg(windows)]
mod pin {
    use std::os::windows::io::RawSocket;

    #[link(name = "ws2_32")]
    extern "system" {
        fn setsockopt(s: usize, level: i32, optname: i32, optval: *const u8, optlen: i32) -> i32;
    }

    const IPPROTO_IP: i32 = 0;
    const IPPROTO_IPV6: i32 = 41;
    const IP_UNICAST_IF: i32 = 31;
    const IPV6_UNICAST_IF: i32 = 31;

    pub fn apply(sock: RawSocket, v6: bool, ifindex: u32) -> std::io::Result<()> {
        // IPv4 expects the index in network byte order, IPv6 in host order.
        let (level, opt, val) = if v6 {
            (IPPROTO_IPV6, IPV6_UNICAST_IF, ifindex)
        } else {
            (IPPROTO_IP, IP_UNICAST_IF, ifindex.to_be())
        };
        let bytes = val.to_ne_bytes();
        let rc = unsafe { setsockopt(sock as usize, level, opt, bytes.as_ptr(), 4) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

fn pin_tcp(_s: &TcpSocket, _v6: bool) -> io::Result<()> {
    let idx = bypass_interface();
    if idx == 0 {
        return Ok(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        pin::apply(_s.as_raw_socket(), _v6, idx)?;
    }
    Ok(())
}

pub async fn tcp_connect(addr: SocketAddr) -> io::Result<TcpStream> {
    let s = if addr.is_ipv4() { TcpSocket::new_v4()? } else { TcpSocket::new_v6()? };
    pin_tcp(&s, addr.is_ipv6())?;
    s.connect(addr).await
}

/// Unconnected UDP socket for direct traffic of the given family.
pub fn udp_socket(v6: bool) -> io::Result<UdpSocket> {
    let bind: SocketAddr = if v6 { "[::]:0".parse().unwrap() } else { "0.0.0.0:0".parse().unwrap() };
    let std_sock = std::net::UdpSocket::bind(bind)?;
    let idx = bypass_interface();
    if idx != 0 {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            pin::apply(std_sock.as_raw_socket(), v6, idx)?;
        }
    }
    std_sock.set_nonblocking(true)?;
    UdpSocket::from_std(std_sock)
}

/// Connect directly to host:port. Uses the OS resolver unless a custom
/// resolver is required (TUN modes, where the OS would answer with fake IPs).
pub async fn connect_host(host: &str, port: u16, resolver: &Resolver) -> io::Result<TcpStream> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return tcp_connect(SocketAddr::new(ip, port)).await;
    }
    if bypass_interface() == 0 && resolver.is_system() {
        return TcpStream::connect((host, port)).await;
    }
    let ips = resolver.resolve(host).await?;
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{host}: no addresses"));
    for ip in ips.into_iter().take(3) {
        match tokio::time::timeout(Duration::from_secs(5), tcp_connect(SocketAddr::new(ip, port))).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => last = e,
            Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, format!("{ip}:{port}: timeout")),
        }
    }
    Err(last)
}
