//! UDP through the hub: the SOCKS5 UDP ASSOCIATE inbound (used by apps and by
//! the sing-box TUN adapter) and per-flow relays (used by the native TUN).
//! UDP goes through the same rules as TCP.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::hub::{self, current, outbound_label, Decision, Routing, SharedRouting};
use crate::resolver::Resolver;
use crate::stats::{ConnEntry, Stats};

const FLOW_IDLE: Duration = Duration::from_secs(120);
const MAX_DESTS: usize = 2048;

/// Parse a SOCKS5 UDP datagram header: (host, port, header length).
pub(crate) fn parse_udp_header(p: &[u8]) -> Option<(String, u16, usize)> {
    if p.len() < 4 || p[2] != 0 {
        return None; // fragmented datagrams are not supported
    }
    let (host, used) = match p[3] {
        1 => {
            let a = p.get(4..8)?;
            (IpAddr::from([a[0], a[1], a[2], a[3]]).to_string(), 4)
        }
        3 => {
            let l = *p.get(4)? as usize;
            let d = p.get(5..5 + l)?;
            (String::from_utf8_lossy(d).into_owned(), 1 + l)
        }
        4 => {
            let a: [u8; 16] = p.get(4..20)?.try_into().ok()?;
            (IpAddr::from(a).to_string(), 16)
        }
        _ => return None,
    };
    let off = 4 + used;
    let port = u16::from_be_bytes([*p.get(off)?, *p.get(off + 1)?]);
    Some((host, port, off + 2))
}

pub(crate) fn udp_header(host: &str, port: u16) -> io::Result<Vec<u8>> {
    let mut h = vec![0u8, 0, 0];
    h.extend_from_slice(&hub::encode_addr(host, port)?);
    Ok(h)
}

/// A UDP association with a local engine inbound.
pub(crate) struct SocksUdp {
    _ctrl: TcpStream,
    pub sock: UdpSocket,
}

impl SocksUdp {
    pub async fn open(engine_port: u16) -> io::Result<Self> {
        let mut ctrl = hub::socks_hello(engine_port).await?;
        ctrl.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        let mut h = [0u8; 4];
        ctrl.read_exact(&mut h).await?;
        if h[1] != 0 {
            return Err(hub::other(format!("engine refused UDP ASSOCIATE (code {})", h[1])));
        }
        let (host, port) = hub::read_addr(&mut ctrl, h[3]).await?;
        // Engines listen on 127.0.0.1; an unspecified or IPv6 relay address means the same host.
        let ip = match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) if !v4.is_unspecified() => IpAddr::V4(v4),
            _ => IpAddr::V4(Ipv4Addr::LOCALHOST),
        };
        let sock = UdpSocket::bind("127.0.0.1:0").await?;
        sock.connect(SocketAddr::new(ip, port)).await?;
        Ok(Self { _ctrl: ctrl, sock })
    }

    pub async fn send_to_target(&self, host: &str, port: u16, payload: &[u8]) -> io::Result<()> {
        let mut pkt = udp_header(host, port)?;
        pkt.extend_from_slice(payload);
        self.sock.send(&pkt).await.map(|_| ())
    }
}

// ---------------------------------------------------------------- SOCKS5 UDP ASSOCIATE inbound

#[derive(Clone)]
enum Target {
    Block,
    Direct(Arc<Resolver>),
    Node(u16),
}

struct Dest {
    target: Target,
    conn: Arc<ConnEntry>,
    addr: Option<SocketAddr>,
    closed: bool,
}

struct NodeOut {
    su: Arc<SocksUdp>,
    last: Arc<Mutex<Arc<ConnEntry>>>,
}

type Reverse = Arc<Mutex<HashMap<SocketAddr, (String, u16, Arc<ConnEntry>)>>>;

struct Session {
    client_sock: Arc<UdpSocket>,
    client: Option<SocketAddr>,
    shared: SharedRouting,
    stats: Arc<Stats>,
    dests: HashMap<String, Dest>,
    direct: [Option<Arc<UdpSocket>>; 2],
    nodes: HashMap<u16, NodeOut>,
    tasks: Vec<JoinHandle<()>>,
    reverse: Reverse,
}

pub async fn associate(mut ctrl: TcpStream, shared: SharedRouting, stats: Arc<Stats>) -> io::Result<()> {
    // Answer with the address the client reached us on (loopback or the LAN address).
    let local_ip = match ctrl.local_addr()?.ip() {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(_) => Ipv4Addr::LOCALHOST,
    };
    let sock = Arc::new(UdpSocket::bind(SocketAddr::new(IpAddr::V4(local_ip), 0)).await?);
    let port = sock.local_addr()?.port();
    let mut rep = vec![5u8, 0, 0, 1];
    rep.extend_from_slice(&local_ip.octets());
    rep.extend_from_slice(&port.to_be_bytes());
    ctrl.write_all(&rep).await?;
    let mut sess = Session {
        client_sock: sock.clone(),
        client: None,
        shared,
        stats,
        dests: HashMap::new(),
        direct: [None, None],
        nodes: HashMap::new(),
        tasks: Vec::new(),
        reverse: Arc::new(Mutex::new(HashMap::new())),
    };
    let mut buf = vec![0u8; 65536];
    let mut cbuf = [0u8; 64];
    loop {
        tokio::select! {
            r = sock.recv_from(&mut buf) => match r {
                Ok((n, from)) => sess.on_packet(&buf[..n], from).await,
                Err(_) => break,
            },
            r = ctrl.read(&mut cbuf) => match r {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            },
        }
    }
    sess.close();
    Ok(())
}

impl Session {
    async fn on_packet(&mut self, pkt: &[u8], from: SocketAddr) {
        match self.client {
            None => self.client = Some(from),
            Some(c) if c != from => return,
            _ => {}
        }
        let client = from;
        let Some((host, port, hl)) = parse_udp_header(pkt) else { return };
        let payload = &pkt[hl..];
        let key = format!("{host}:{port}");

        if !self.dests.contains_key(&key) {
            if self.dests.len() >= MAX_DESTS {
                return;
            }
            let routing = current(&self.shared);
            let (decision, rule) = routing.decide_full(&host, port).await;
            let (outbound, engine) = outbound_label(&decision);
            let conn = self.stats.open(&host, port, "udp", rule, outbound, engine);
            let (target, closed) = match decision {
                Decision::Block => {
                    self.stats.close(&conn, Some("blocked by rule".into()));
                    (Target::Block, true)
                }
                Decision::Direct => (Target::Direct(routing.resolver.clone()), false),
                Decision::Via(u) => (Target::Node(u.port), false),
            };
            self.dests.insert(key.clone(), Dest { target, conn, addr: None, closed });
        }
        let (target, conn, addr) = match self.dests.get(&key) {
            Some(d) => (d.target.clone(), d.conn.clone(), d.addr),
            None => return,
        };

        match target {
            Target::Block => {}
            Target::Direct(resolver) => {
                let addr = match addr {
                    Some(a) => a,
                    None => {
                        let ip = match host.parse::<IpAddr>() {
                            Ok(ip) => Some(ip),
                            Err(_) => resolver.resolve(&host).await.ok().and_then(|v| v.into_iter().next()),
                        };
                        let Some(ip) = ip else { return };
                        let a = SocketAddr::new(ip, port);
                        if let Some(d) = self.dests.get_mut(&key) {
                            d.addr = Some(a);
                        }
                        self.reverse.lock().unwrap().insert(a, (host.clone(), port, conn.clone()));
                        a
                    }
                };
                let Ok(sock) = self.direct_socket(addr.is_ipv6(), client) else { return };
                if sock.send_to(payload, addr).await.is_ok() {
                    self.stats.add(&conn, payload.len() as u64, true);
                }
            }
            Target::Node(engine_port) => {
                if !self.nodes.contains_key(&engine_port) {
                    let Ok(su) = SocksUdp::open(engine_port).await else { return };
                    let su = Arc::new(su);
                    let last = Arc::new(Mutex::new(conn.clone()));
                    let (rsu, rlast, cs, stats) = (su.clone(), last.clone(), self.client_sock.clone(), self.stats.clone());
                    self.tasks.push(tokio::spawn(async move {
                        let mut buf = vec![0u8; 65536];
                        // Engine replies already carry a SOCKS UDP header: forward as is.
                        while let Ok(n) = rsu.sock.recv(&mut buf).await {
                            if cs.send_to(&buf[..n], client).await.is_err() {
                                break;
                            }
                            let c = rlast.lock().unwrap().clone();
                            stats.add(&c, n as u64, false);
                        }
                    }));
                    self.nodes.insert(engine_port, NodeOut { su, last });
                }
                let Some(node) = self.nodes.get(&engine_port) else { return };
                *node.last.lock().unwrap() = conn.clone();
                let su = node.su.clone();
                if su.send_to_target(&host, port, payload).await.is_ok() {
                    self.stats.add(&conn, payload.len() as u64, true);
                }
            }
        }
    }

    fn direct_socket(&mut self, v6: bool, client: SocketAddr) -> io::Result<Arc<UdpSocket>> {
        let slot = if v6 { 1 } else { 0 };
        if let Some(s) = &self.direct[slot] {
            return Ok(s.clone());
        }
        let s = Arc::new(crate::net::udp_socket(v6)?);
        let (rs, cs, rev, stats) = (s.clone(), self.client_sock.clone(), self.reverse.clone(), self.stats.clone());
        self.tasks.push(tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok((n, src)) = rs.recv_from(&mut buf).await {
                let found = rev.lock().unwrap().get(&src).map(|(h, p, c)| (h.clone(), *p, c.clone()));
                let (host, port, conn) = match found {
                    Some((h, p, c)) => (h, p, Some(c)),
                    None => (src.ip().to_string(), src.port(), None),
                };
                let Ok(mut pkt) = udp_header(&host, port) else { continue };
                pkt.extend_from_slice(&buf[..n]);
                if cs.send_to(&pkt, client).await.is_err() {
                    break;
                }
                if let Some(c) = conn {
                    stats.add(&c, n as u64, false);
                }
            }
        }));
        self.direct[slot] = Some(s.clone());
        Ok(s)
    }

    fn close(self) {
        for t in &self.tasks {
            t.abort();
        }
        for d in self.dests.values() {
            if !d.closed {
                self.stats.close(&d.conn, None);
            }
        }
    }
}

// ---------------------------------------------------------------- per-flow relay (native TUN)

/// Relay one UDP flow (a stream where every read/write is one datagram).
pub async fn relay_flow<S>(flow: S, host: String, port: u16, routing: Arc<Routing>, stats: Arc<Stats>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (decision, rule) = routing.decide_full(&host, port).await;
    let (outbound, engine) = outbound_label(&decision);
    let conn = stats.open(&host, port, "udp", rule, outbound, engine);
    let res = flow_inner(flow, &host, port, decision, &routing, &conn, &stats).await;
    stats.close(&conn, res.as_ref().err().map(|e| e.to_string()));
    res
}

async fn flow_inner<S>(
    mut flow: S,
    host: &str,
    port: u16,
    decision: Decision,
    routing: &Routing,
    conn: &Arc<ConnEntry>,
    stats: &Arc<Stats>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; 65536];
    let mut rbuf = vec![0u8; 65536];
    match decision {
        Decision::Block => Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked by rule")),
        Decision::Direct => {
            let ip = match host.parse::<IpAddr>() {
                Ok(ip) => ip,
                Err(_) => routing.resolver.resolve(host).await?.into_iter().next().ok_or_else(|| hub::other("no address"))?,
            };
            let sock = crate::net::udp_socket(ip.is_ipv6())?;
            sock.connect(SocketAddr::new(ip, port)).await?;
            loop {
                tokio::select! {
                    r = timeout(FLOW_IDLE, flow.read(&mut buf)) => {
                        let n = match r {
                            Ok(Ok(0)) | Err(_) => return Ok(()),
                            Ok(Ok(n)) => n,
                            Ok(Err(e)) => return Err(e),
                        };
                        sock.send(&buf[..n]).await?;
                        stats.add(conn, n as u64, true);
                    }
                    r = sock.recv(&mut rbuf) => {
                        let n = r?;
                        flow.write_all(&rbuf[..n]).await?;
                        stats.add(conn, n as u64, false);
                    }
                }
            }
        }
        Decision::Via(u) => {
            let su = SocksUdp::open(u.port).await?;
            loop {
                tokio::select! {
                    r = timeout(FLOW_IDLE, flow.read(&mut buf)) => {
                        let n = match r {
                            Ok(Ok(0)) | Err(_) => return Ok(()),
                            Ok(Ok(n)) => n,
                            Ok(Err(e)) => return Err(e),
                        };
                        su.send_to_target(host, port, &buf[..n]).await?;
                        stats.add(conn, n as u64, true);
                    }
                    r = su.sock.recv(&mut rbuf) => {
                        let n = r?;
                        if let Some((_, _, hl)) = parse_udp_header(&rbuf[..n]) {
                            flow.write_all(&rbuf[hl..n]).await?;
                            stats.add(conn, (n - hl) as u64, false);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        for (h, p) in [("example.com", 53u16), ("192.0.2.1", 443), ("2001:db8::5", 8443)] {
            let mut pkt = udp_header(h, p).unwrap();
            let hl = pkt.len();
            pkt.extend_from_slice(b"payload");
            let (host, port, len) = parse_udp_header(&pkt).unwrap();
            assert_eq!((host.as_str(), port, len), (h, p, hl));
            assert_eq!(&pkt[len..], b"payload");
        }
        assert!(parse_udp_header(&[0, 0, 1, 1, 1, 2, 3, 4, 0, 53]).is_none(), "fragments rejected");
    }
}
