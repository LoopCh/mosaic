//! DNS resolution for the hub's own needs: direct connections in TUN modes
//! and IP rules (geoip / ip_cidr) applied to domain destinations.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::dns;

const CACHE_TTL: Duration = Duration::from_secs(120);

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
        let s = s.trim();
        let addr = s
            .parse::<SocketAddr>()
            .ok()
            .or_else(|| s.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 53)))
            .unwrap_or_else(|| "8.8.8.8:53".parse().unwrap());
        Self::udp(addr)
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
