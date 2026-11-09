//! DNS resolution for the hub's own needs: direct connections in TUN modes
//! and IP rules (geoip / ip_cidr) applied to domain destinations.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::dns;

const CACHE_TTL: Duration = Duration::from_secs(120);

/// Parse "1.1.1.1", "1.1.1.1:53", "[2606:4700::1111]:53"; `default` if invalid.
pub fn parse_dns_server(s: &str, default: &str) -> SocketAddr {
    let s = s.trim();
    s.parse::<SocketAddr>()
        .ok()
        .or_else(|| s.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 53)))
        .unwrap_or_else(|| default.parse().unwrap())
}

/// Small TTL cache shared by remote lookups.
#[derive(Default)]
pub struct DnsCache(Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>);

impl DnsCache {
    pub fn get(&self, host: &str) -> Option<Vec<IpAddr>> {
        let g = self.0.lock().unwrap();
        g.get(host).filter(|(_, at)| at.elapsed() < CACHE_TTL).map(|(v, _)| v.clone())
    }

    pub fn put(&self, host: &str, ips: Vec<IpAddr>) {
        let mut g = self.0.lock().unwrap();
        if g.len() > 4096 {
            g.clear();
        }
        g.insert(host.to_string(), (ips, Instant::now()));
    }
}

/// One DNS exchange over TCP (RFC 1035 length-prefixed) through a local
/// engine SOCKS inbound — i.e. through the tunnel.
pub async fn exchange_via_socks(engine_port: u16, server: SocketAddr, query: &[u8]) -> io::Result<Vec<u8>> {
    let mut s = crate::hub::socks_connect(engine_port, &server.ip().to_string(), server.port()).await?;
    let mut msg = Vec::with_capacity(query.len() + 2);
    msg.extend_from_slice(&(query.len() as u16).to_be_bytes());
    msg.extend_from_slice(query);
    s.write_all(&msg).await?;
    let len = s.read_u16().await? as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await?;
    Ok(buf)
}

pub enum Upstream {
    /// Operating-system resolver.
    System,
    /// Plain DNS over UDP to this server (sockets follow the TUN bypass).
    Udp(SocketAddr),
}

pub struct Resolver {
    upstream: Upstream,
    cache: Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>,
}

impl Resolver {
    pub fn system() -> Self {
        Self { upstream: Upstream::System, cache: Mutex::new(HashMap::new()) }
    }

    pub fn udp(server: SocketAddr) -> Self {
        Self { upstream: Upstream::Udp(server), cache: Mutex::new(HashMap::new()) }
    }

    /// Parse "8.8.8.8", "1.1.1.1:53" or "[2606:4700::1111]:53"; falls back to 8.8.8.8.
    pub fn udp_from_setting(s: &str) -> Self {
        Self::udp(parse_dns_server(s, "8.8.8.8:53"))
    }

    pub fn is_system(&self) -> bool {
        matches!(self.upstream, Upstream::System)
    }

    pub fn server(&self) -> Option<SocketAddr> {
        match self.upstream {
            Upstream::Udp(a) => Some(a),
            Upstream::System => None,
        }
    }

    pub async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        if let Ok(ip) = host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        let key = host.to_ascii_lowercase();
        if let Some((ips, at)) = self.cache.lock().unwrap().get(&key) {
            if at.elapsed() < CACHE_TTL {
                return Ok(ips.clone());
            }
        }
        let ips: Vec<IpAddr> = match self.upstream {
            Upstream::System => tokio::net::lookup_host((key.as_str(), 0)).await?.map(|a| a.ip()).collect(),
            Upstream::Udp(server) => {
                let mut v = query(server, &key, dns::TYPE_A).await.unwrap_or_default();
                if v.is_empty() {
                    v = query(server, &key, dns::TYPE_AAAA).await?;
                }
                v
            }
        };
        if ips.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host}: no addresses")));
        }
        let mut c = self.cache.lock().unwrap();
        if c.len() > 4096 {
            c.clear();
        }
        c.insert(key, (ips.clone(), Instant::now()));
        Ok(ips)
    }
}

async fn query(server: SocketAddr, name: &str, qtype: u16) -> io::Result<Vec<IpAddr>> {
    let sock = crate::net::udp_socket(server.is_ipv6())?;
    let id = (crate::util::now_ms() as u16) ^ (crate::util::fnv64(name) as u16);
    let q = dns::build_query(id, name, qtype);
    let mut buf = vec![0u8; 1500];
    for _ in 0..2 {
        sock.send_to(&q, server).await?;
        let deadline = Instant::now() + Duration::from_millis(2500);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
                Ok(Ok((n, from))) if from == server => {
                    if let Some((rid, rcode, addrs)) = dns::parse_response(&buf[..n]) {
                        if rid == id {
                            if rcode != 0 && rcode != 3 {
                                return Err(io::Error::new(io::ErrorKind::Other, format!("DNS rcode {rcode}")));
                            }
                            return Ok(addrs.into_iter().map(|(ip, _)| ip).collect());
                        }
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => break,
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, format!("DNS {server}: timeout")))
}
