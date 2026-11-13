//! The hub data plane: one mixed inbound (SOCKS5 incl. UDP ASSOCIATE + HTTP
//! proxy) on loopback, a router that evaluates rules, and a dispatcher that
//! connects either directly or through the local SOCKS inbound of the engine
//! serving a node.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::geo::GeoData;
use crate::dns;
use crate::model::{DomainStrategy, EngineKind, IpRuleDns, Rule, RuleKind};
use crate::resolver::{self, DnsCache, Resolver};
use crate::rules;
use crate::stats::{ConnEntry, Stats};
use crate::util::LogBuf;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const BUF: usize = 16 * 1024;
/// A TCP connection with no data in either direction for this long is closed
/// (same as Xray's default `connIdle`).
const TCP_IDLE: Duration = Duration::from_secs(300);
/// After one side has finished sending, the other gets this long without data.
const TCP_HALF_CLOSED_IDLE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct Upstream {
    pub port: u16,
    pub name: String,
    pub engine: EngineKind,
    /// The node carries UDP (an HTTP proxy does not).
    pub udp: bool,
}

#[derive(Clone, Debug)]
pub enum Decision {
    Direct,
    Block,
    Via(Upstream),
}

pub enum Verdict {
    Done(Decision, String),
    /// No domain rule matched and the strategy asks to retry IP rules with a resolved address.
    NeedIp,
}

/// Immutable routing table; swapped atomically on every change.
#[derive(Clone)]
pub struct Routing {
    pub rules: Vec<Rule>,
    pub final_target: String,
    pub selected: Option<String>,
    pub upstreams: HashMap<String, Upstream>,
    /// sing-box TUN adapter mode: "direct" goes through this SOCKS port of the
    /// adapter, because the system resolver would hand the hub fake IPs.
    pub direct_via: Option<u16>,
    pub geo: Arc<GeoData>,
    pub strategy: DomainStrategy,
    /// Local/direct DNS.
    pub resolver: Arc<Resolver>,
    /// DNS server reached through the active node.
    pub remote_dns: SocketAddr,
    pub ip_rule_dns: IpRuleDns,
    pub remote_cache: Arc<DnsCache>,
    /// See `Settings::webrtc_protection`.
    pub webrtc_guard: bool,
    /// For WebRTC events (what the protection did, once a minute per server).
    pub logs: Option<Arc<LogBuf>>,
}

/// WebRTC events already logged (per server, once a minute).
fn stun_seen() -> &'static crate::util::Throttle {
    static SEEN: std::sync::OnceLock<crate::util::Throttle> = std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

pub type SharedRouting = Arc<RwLock<Arc<Routing>>>;

impl Routing {
    /// Minimal routing table (used by tests and as a placeholder).
    pub fn basic(rules: Vec<Rule>, final_target: &str) -> Self {
        Routing {
            rules,
            final_target: final_target.to_string(),
            selected: None,
            upstreams: HashMap::new(),
            direct_via: None,
            geo: Arc::new(GeoData::default()),
            strategy: DomainStrategy::AsIs,
            resolver: Arc::new(Resolver::system()),
            remote_dns: "1.1.1.1:53".parse().unwrap(),
            ip_rule_dns: IpRuleDns::Remote,
            remote_cache: Arc::new(DnsCache::default()),
            webrtc_guard: false,
            logs: None,
        }
    }

    /// WebRTC leak protection: a STUN request ("what is my address?") that a rule would
    /// send direct goes through the active node instead (or is blocked if there is none),
    /// so pages cannot learn the real IP. Calls themselves keep working through the node.
    /// Checks between devices of the local network stay local.
    /// The final say over a UDP flow: WebRTC protection, then whether the chosen node can
    /// carry UDP at all — an HTTP proxy cannot, and a quick refusal lets the app fall back to
    /// TCP (QUIC → HTTPS) instead of waiting for packets that go nowhere.
    pub fn guard_udp(&self, decided: (Decision, String), host: &str, port: u16, process: Option<&str>, payload: &[u8]) -> (Decision, String) {
        let (d, rule) = self.stun_guard(decided, host, port, process, payload);
        match &d {
            Decision::Via(u) if !u.udp => {
                if let Some(l) = &self.logs {
                    if stun_seen().allow(&format!("no-udp|{}|{host}:{port}", u.name), 60_000) {
                        let who = process.map(|p| format!("{p} → ")).unwrap_or_default();
                        l.warn(format!(
                            "[conn] UDP {who}{host}:{port} not sent: '{}' is an HTTP proxy, it carries TCP only — the app falls back to TCP (QUIC → HTTPS)",
                            u.name
                        ));
                    }
                }
                (Decision::Block, format!("{rule} · no UDP via {}", u.name))
            }
            _ => (d, rule),
        }
    }

    fn stun_guard(&self, decided: (Decision, String), host: &str, port: u16, process: Option<&str>, payload: &[u8]) -> (Decision, String) {
        let local = match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(v4)) => v4.is_private() || v4.is_loopback() || v4.is_link_local() || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64),
            Ok(IpAddr::V6(v6)) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
            Err(_) => host.ends_with(".local"),
        };
        if local || !matches!(decided.0, Decision::Direct) || !crate::sniff::is_stun(payload) {
            return decided;
        }
        // A STUN request (WebRTC asking "what is my address?", or a call checking a path)
        // that the rules send direct.
        let who = process.map(|p| format!("{p} → ")).unwrap_or_default();
        let say = |msg: String| {
            if let Some(l) = &self.logs {
                if stun_seen().allow(&format!("{host}:{port}|{}", self.webrtc_guard), 60_000) {
                    l.push(msg);
                }
            }
        };
        if !self.webrtc_guard {
            say(format!("[webrtc] STUN {who}{host}:{port} goes direct (rule {}): WebRTC protection is off — the page can learn your real IP", decided.1));
            return decided;
        }
        let guarded = self.target("proxy");
        match &guarded {
            Decision::Block => say(format!("[webrtc] STUN {who}{host}:{port} blocked by WebRTC protection: no active node to send it through — calls may not connect")),
            _ => say(format!(
                "[webrtc] STUN {who}{host}:{port} sent through the node instead of direct (rule {}) by WebRTC protection; \
                 if calls fail, check that the node supports UDP or turn the protection off",
                decided.1
            )),
        }
        (guarded, "WebRTC protection".into())
    }

    /// Raw DNS exchange through the active node (the query ID is preserved).
    pub async fn remote_exchange(&self, query: &[u8]) -> io::Result<Vec<u8>> {
        let u = self
            .selected
            .as_ref()
            .and_then(|id| self.upstreams.get(id))
            .ok_or_else(|| other("no active node for remote DNS"))?;
        match timeout(Duration::from_secs(5), resolver::exchange_via_socks(u.port, self.remote_dns, query)).await {
            Ok(r) => r,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "remote DNS timeout")),
        }
    }

    /// Resolve through the tunnel (A, then AAAA), cached.
    pub async fn remote_resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let key = host.to_ascii_lowercase();
        if let Some(v) = self.remote_cache.get(&key) {
            return Ok(v);
        }
        let mut out = Vec::new();
        for qtype in [dns::TYPE_A, dns::TYPE_AAAA] {
            let id = (crate::util::now_ms() as u16) ^ qtype;
            let resp = self.remote_exchange(&dns::build_query(id, &key, qtype)).await?;
            if let Some((_, _, addrs)) = dns::parse_response(&resp) {
                out.extend(addrs.into_iter().map(|(ip, _)| ip));
            }
            if !out.is_empty() {
                break;
            }
        }
        if out.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host}: no addresses")));
        }
        self.remote_cache.put(&key, out.clone());
        Ok(out)
    }

    /// True if some enabled rule matches on the application name.
    pub fn needs_process(&self) -> bool {
        self.rules.iter().any(|r| r.enabled && r.kind == RuleKind::ProcessName)
    }

    /// `resolved_ip` is set on the second pass after a NeedIp verdict.
    /// `process` is the application's executable name, when known.
    pub fn decide(&self, host: &str, port: u16, resolved_ip: Option<IpAddr>, process: Option<&str>) -> Verdict {
        let h = host.to_ascii_lowercase();
        let literal: Option<IpAddr> = h.trim_matches(|c| c == '[' || c == ']').parse().ok();
        let ip = literal.or(resolved_ip);
        for (i, r) in self.rules.iter().enumerate() {
            if r.enabled && rules::matches(r, &h, ip, port, process, &self.geo) {
                let mut v = r.value.clone();
                if v.chars().count() > 40 {
                    v = v.chars().take(40).collect::<String>() + "…";
                }
                return Verdict::Done(self.target(&r.target), format!("#{} {} {}", i + 1, r.kind.label(), v));
            }
        }
        let has_ip_rules = self.rules.iter().any(|r| r.enabled && r.kind.needs_ip());
        if self.strategy == DomainStrategy::IpIfNonMatch && ip.is_none() && resolved_ip.is_none() && has_ip_rules {
            return Verdict::NeedIp;
        }
        Verdict::Done(self.target(&self.final_target), "final".into())
    }

    /// Full decision including the resolve-and-retry step.
    pub async fn decide_full(&self, host: &str, port: u16, process: Option<&str>) -> (Decision, String) {
        match self.decide(host, port, None, process) {
            Verdict::Done(d, r) => (d, r),
            Verdict::NeedIp => {
                let found = match self.ip_rule_dns {
                    IpRuleDns::Remote => self.remote_resolve(host).await,
                    IpRuleDns::Local => self.resolver.resolve(host).await,
                };
                let ip = found.ok().and_then(|v| v.into_iter().next());
                match self.decide(host, port, ip.or(Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED))), process) {
                    Verdict::Done(d, r) => (d, r),
                    Verdict::NeedIp => (self.target(&self.final_target), "final".into()),
                }
            }
        }
    }

    fn target(&self, t: &str) -> Decision {
        match t {
            "direct" => Decision::Direct,
            "block" => Decision::Block,
            "proxy" => match &self.selected {
                Some(id) => self.via(id),
                None => Decision::Block,
            },
            other => match other.strip_prefix("node:") {
                Some(id) => self.via(id),
                None => Decision::Block,
            },
        }
    }

    fn via(&self, id: &str) -> Decision {
        self.upstreams.get(id).map(|u| Decision::Via(u.clone())).unwrap_or(Decision::Block)
    }
}

pub fn outbound_label(d: &Decision) -> (String, Option<String>) {
    match d {
        Decision::Direct => ("direct".to_string(), None),
        Decision::Block => ("block".to_string(), None),
        Decision::Via(u) => (u.name.clone(), Some(u.engine.label().to_string())),
    }
}

pub struct Hub {
    pub port: u16,
    task: JoinHandle<()>,
    routing: SharedRouting,
}

impl Hub {
    /// `allow_lan`: listen on all interfaces instead of loopback only.
    pub async fn start(port: u16, allow_lan: bool, routing: Routing, stats: Arc<Stats>, logs: Arc<LogBuf>) -> Result<Hub, String> {
        let host = if allow_lan { "0.0.0.0" } else { "127.0.0.1" };
        let listener = TcpListener::bind((host, port))
            .await
            .map_err(|e| format!("{host}:{port} is busy or unavailable: {e}"))?;
        let routing: SharedRouting = Arc::new(RwLock::new(Arc::new(routing)));
        let shared = routing.clone();
        logs.push(format!("[hub] listening on {host}:{port} (SOCKS5 + HTTP)"));
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((sock, _)) => {
                        let shared = shared.clone();
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            let _ = handle(sock, shared, stats).await;
                        });
                    }
                    Err(e) => {
                        logs.push(format!("[hub] accept: {e}"));
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        });
        Ok(Hub { port, task, routing })
    }

    pub fn update(&self, routing: Routing) {
        *self.routing.write().unwrap() = Arc::new(routing);
    }

    pub fn shared(&self) -> SharedRouting {
        self.routing.clone()
    }

    pub fn stop(self) {
        self.task.abort();
    }
}

pub fn current(shared: &SharedRouting) -> Arc<Routing> {
    shared.read().unwrap().clone()
}

pub(crate) fn other(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.into())
}

/// Executable name of the local application on the other end of `s`
/// (shown in the connection journal and used by per-app rules).
pub(crate) fn peer_process(s: &TcpStream, _routing: &Routing) -> Option<String> {
    let peer = s.peer_addr().ok()?;
    if !peer.ip().is_loopback() {
        return None; // another device in the LAN
    }
    // Through the sing-box TUN adapter every connection comes from sing-box itself.
    crate::procinfo::process_of(peer, crate::procinfo::Proto::Tcp).filter(|p| !p.starts_with("sing-box"))
}

async fn handle(mut s: TcpStream, shared: SharedRouting, stats: Arc<Stats>) -> io::Result<()> {
    let _ = s.set_nodelay(true);
    let first = s.read_u8().await?;
    let process = peer_process(&s, &current(&shared));
    if first == 5 {
        socks5(s, shared, &stats, process).await
    } else {
        let routing = current(&shared);
        http(s, first, &routing, &stats, process.as_deref()).await
    }
}

// ---------------------------------------------------------------- SOCKS5

async fn socks5(mut s: TcpStream, shared: SharedRouting, stats: &Arc<Stats>, process: Option<String>) -> io::Result<()> {
    let n = s.read_u8().await? as usize;
    let mut methods = vec![0u8; n];
    s.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        s.write_all(&[5, 0xff]).await?;
        return Ok(());
    }
    s.write_all(&[5, 0]).await?;
    let mut h = [0u8; 4];
    s.read_exact(&mut h).await?;
    let (host, port) = read_addr(&mut s, h[3]).await?;
    match h[1] {
        1 => {
            let routing = current(&shared);
            match connect_target(&host, port, process.as_deref(), &routing, stats).await {
                Ok((up, conn)) => {
                    s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                    relay(s, up, conn, stats.clone()).await
                }
                Err(e) => {
                    let code = if e.kind() == io::ErrorKind::PermissionDenied { 2 } else { 5 };
                    let _ = s.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await;
                    Err(e)
                }
            }
        }
        3 => crate::udp::associate(s, shared, stats.clone(), process).await,
        _ => {
            s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            Ok(())
        }
    }
}

pub(crate) async fn read_addr<R: AsyncRead + Unpin>(r: &mut R, atyp: u8) -> io::Result<(String, u16)> {
    let host = match atyp {
        1 => {
            let mut a = [0u8; 4];
            r.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let len = r.read_u8().await? as usize;
            let mut d = vec![0u8; len];
            r.read_exact(&mut d).await?;
            String::from_utf8_lossy(&d).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            r.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => return Err(other("socks: unknown address type")),
    };
    let port = r.read_u16().await?;
    Ok((host, port))
}

/// SOCKS5 address encoding (ATYP + address + port).
pub(crate) fn encode_addr(host: &str, port: u16) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(host.len() + 4);
    match host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            out.push(1);
            out.extend_from_slice(&a.octets());
        }
        Ok(IpAddr::V6(a)) => {
            out.push(4);
            out.extend_from_slice(&a.octets());
        }
        Err(_) => {
            let b = host.as_bytes();
            if b.len() > 255 {
                return Err(other("domain name too long"));
            }
            out.push(3);
            out.push(b.len() as u8);
            out.extend_from_slice(b);
        }
    }
    out.extend_from_slice(&port.to_be_bytes());
    Ok(out)
}

/// SOCKS5 handshake with a local engine inbound; returns the stream positioned
/// after the method negotiation.
pub(crate) async fn socks_hello(proxy_port: u16) -> io::Result<TcpStream> {
    let mut s = TcpStream::connect(("127.0.0.1", proxy_port)).await?;
    let _ = s.set_nodelay(true);
    s.write_all(&[5, 1, 0]).await?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await?;
    if r != [5, 0] {
        return Err(other("socks: engine rejected the handshake"));
    }
    Ok(s)
}

/// SOCKS5 CONNECT through a local engine inbound.
pub async fn socks_connect(proxy_port: u16, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut s = socks_hello(proxy_port).await?;
    let mut req = vec![5u8, 1, 0];
    req.extend_from_slice(&encode_addr(host, port)?);
    s.write_all(&req).await?;
    let mut h = [0u8; 4];
    s.read_exact(&mut h).await?;
    if h[1] != 0 {
        return Err(other(format!("engine returned SOCKS error {}", h[1])));
    }
    read_addr(&mut s, h[3]).await?;
    Ok(s)
}

// ---------------------------------------------------------------- HTTP proxy

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub(crate) fn split_hp(s: &str, default_port: u16) -> io::Result<(String, u16)> {
    if let Some(r) = s.strip_prefix('[') {
        let (h, p) = r.split_once(']').ok_or_else(|| other("bad address"))?;
        let port = p.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
        return Ok((h.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Ok((h.to_string(), p.parse().map_err(|_| other("bad port"))?)),
        _ => Ok((s.to_string(), default_port)),
    }
}

async fn http(mut s: TcpStream, first: u8, routing: &Routing, stats: &Arc<Stats>, process: Option<&str>) -> io::Result<()> {
    let mut buf = vec![first];
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > 64 * 1024 {
            return Err(other("HTTP header too large"));
        }
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let rest = buf[head_end..].to_vec();
    let mut lines = head.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_hp(&target, 443)?;
        match connect_target(&host, port, process, routing, stats).await {
            Ok((mut up, conn)) => {
                s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
                if !rest.is_empty() {
                    up.write_all(&rest).await?;
                    stats.add(&conn, rest.len() as u64, true);
                }
                relay(s, up, conn, stats.clone()).await
            }
            Err(e) => {
                let _ = s.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                Err(e)
            }
        }
    } else {
        let Some(without) = target.strip_prefix("http://") else {
            let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            return Ok(());
        };
        let (authority, path) = match without.find('/') {
            Some(i) => (&without[..i], &without[i..]),
            None => (without, "/"),
        };
        let (host, port) = split_hp(authority, 80)?;
        let mut new_head = format!("{method} {path} {version}\r\n");
        for l in lines {
            if l.is_empty() {
                continue;
            }
            let lower = l.to_ascii_lowercase();
            if lower.starts_with("proxy-connection:")
                || lower.starts_with("proxy-authorization:")
                || lower.starts_with("connection:")
                || lower.starts_with("keep-alive:")
            {
                continue;
            }
            new_head.push_str(l);
            new_head.push_str("\r\n");
        }
        // One request per upstream connection keeps the MVP simple and correct.
        new_head.push_str("Connection: close\r\n\r\n");
        match connect_target(&host, port, process, routing, stats).await {
            Ok((mut up, conn)) => {
                up.write_all(new_head.as_bytes()).await?;
                up.write_all(&rest).await?;
                stats.add(&conn, (new_head.len() + rest.len()) as u64, true);
                relay(s, up, conn, stats.clone()).await
            }
            Err(e) => {
                let _ = s.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------- dispatch

/// Decide and open the outbound TCP connection for host:port. Registers the
/// connection in the journal (closed immediately on failure or block).
pub async fn connect_target(
    host: &str,
    port: u16,
    process: Option<&str>,
    routing: &Routing,
    stats: &Arc<Stats>,
) -> io::Result<(TcpStream, Arc<ConnEntry>)> {
    let (decision, rule) = routing.decide_full(host, port, process).await;
    let (outbound, engine) = outbound_label(&decision);
    let conn = stats.open(host, port, "tcp", process, rule, outbound, engine);
    let res = match decision {
        Decision::Block => Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked by rule")),
        Decision::Direct => {
            let fut = async {
                match routing.direct_via {
                    Some(p) => socks_connect(p, host, port).await,
                    None => crate::net::connect_host(host, port, &routing.resolver).await,
                }
            };
            match timeout(CONNECT_TIMEOUT, fut).await {
                Ok(r) => r,
                Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "connect timeout")),
            }
        }
        Decision::Via(u) => match timeout(CONNECT_TIMEOUT, socks_connect(u.port, host, port)).await {
            Ok(r) => r,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "timeout via engine")),
        },
    };
    match res {
        Ok(up) => {
            let _ = up.set_nodelay(true);
            Ok((up, conn))
        }
        Err(e) => {
            stats.close(&conn, Some(e.to_string()));
            Err(e)
        }
    }
}

/// Last activity of a relayed connection, shared by both directions.
struct Activity {
    start: std::time::Instant,
    last_ms: std::sync::atomic::AtomicU64,
    half_closed: std::sync::atomic::AtomicBool,
}

impl Activity {
    fn new() -> Self {
        Self { start: std::time::Instant::now(), last_ms: 0.into(), half_closed: false.into() }
    }
    fn touch(&self) {
        self.last_ms.store(self.start.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    fn idle(&self) -> Duration {
        let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
        self.start.elapsed().saturating_sub(Duration::from_millis(last))
    }
}

/// Copy both ways between any client stream and the upstream TCP connection.
/// Idle connections (and half-closed ones whose other side went quiet) are closed,
/// so sockets do not pile up.
pub async fn relay<S>(client: S, up: TcpStream, conn: Arc<ConnEntry>, stats: Arc<Stats>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (cr, cw) = tokio::io::split(client);
    let (ur, uw) = up.into_split();
    let act = Arc::new(Activity::new());
    let work = async {
        tokio::try_join!(
            pipe(cr, uw, conn.clone(), stats.clone(), true, act.clone()),
            pipe(ur, cw, conn.clone(), stats.clone(), false, act.clone())
        )
    };
    let watchdog = async {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let limit = if act.half_closed.load(std::sync::atomic::Ordering::Relaxed) { TCP_HALF_CLOSED_IDLE } else { TCP_IDLE };
            if act.idle() >= limit {
                return;
            }
        }
    };
    // Dropping `work` drops both halves of both sockets, which closes them.
    let err = tokio::select! {
        r = work => r.err().map(|e| e.to_string()),
        _ = watchdog => None,
    };
    stats.close(&conn, err);
    Ok(())
}

async fn pipe<R, W>(mut r: R, mut w: W, conn: Arc<ConnEntry>, stats: Arc<Stats>, upload: bool, act: Arc<Activity>) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; BUF];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            act.half_closed.store(true, std::sync::atomic::Ordering::Relaxed);
            act.touch();
            let _ = w.shutdown().await;
            return Ok(());
        }
        w.write_all(&buf[..n]).await?;
        act.touch();
        stats.add(&conn, n as u64, upload);
    }
}

/// Latency test through an engine inbound: time from SOCKS CONNECT to the
/// first byte of the HTTP response (includes the proxy handshake).
/// Connection time in ms (see `url_probe`): used by Smart Connect.
pub async fn url_test(proxy_port: u16, url: &str) -> Result<u32, String> {
    url_probe(proxy_port, url).await.map(|p| p.0)
}

/// What a finished HTTP response head tells us about reusing the connection.
#[derive(Debug, PartialEq)]
pub(crate) struct HeadInfo {
    pub status: u16,
    pub content_length: Option<usize>,
    pub close: bool,
}

pub(crate) fn parse_head(head: &[u8]) -> Option<HeadInfo> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let (mut content_length, mut close) = (None, false);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
        if k == "content-length" {
            content_length = v.parse().ok();
        } else if k == "connection" && v.contains("close") {
            close = true;
        } else if k == "transfer-encoding" {
            close = true; // chunked body: not worth parsing for a probe
        }
    }
    if status == 204 || status == 304 {
        content_length = Some(0);
    }
    Some(HeadInfo { status, content_length, close })
}

/// Latency probe through an engine inbound, two numbers:
///   * connection — time to the first byte of a response on a new connection
///     (TCP + TLS/REALITY handshakes with the node, the node connecting to the
///     test site, the request): what every new connection costs;
///   * round trip — a second request on the same connection: one round trip
///     through the tunnel, comparable with "ping" in speed tests.
pub async fn url_probe(proxy_port: u16, url: &str) -> Result<(u32, Option<u32>), String> {
    let rest = url.strip_prefix("http://").ok_or("latency test supports http:// URLs only")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_hp(authority, 80).map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mosaic\r\nConnection: keep-alive\r\n\r\n");
    let started = std::time::Instant::now();
    let first = async {
        let mut s = socks_connect(proxy_port, &host, port).await?;
        s.write_all(req.as_bytes()).await?;
        let mut buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 2048];
        let mut ttfb = None;
        let head_end = loop {
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Err(other("server closed the connection"));
            }
            ttfb.get_or_insert(started.elapsed().as_millis() as u32);
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = find(&buf, b"\r\n\r\n") {
                break Some(i + 4);
            }
            if buf.len() > 16 * 1024 {
                break None;
            }
        };
        let ms = ttfb.unwrap_or(0);
        // Can the connection carry a second request? Then drain the body first.
        let info = head_end.and_then(|e| parse_head(&buf[..e]).map(|h| (e, h)));
        let reusable = match info {
            Some((end, h)) if !h.close => match h.content_length {
                Some(len) if len <= 64 * 1024 => {
                    let mut have = buf.len() - end;
                    while have < len {
                        let n = s.read(&mut chunk).await?;
                        if n == 0 {
                            break;
                        }
                        have += n;
                    }
                    have == len
                }
                _ => false,
            },
            _ => false,
        };
        Ok::<_, io::Error>((ms, reusable.then_some(s)))
    };
    let (ms, conn) = match timeout(Duration::from_secs(8), first).await {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err("timeout".into()),
    };
    let rtt = match conn {
        Some(mut s) => {
            let second = async {
                let t = std::time::Instant::now();
                s.write_all(req.as_bytes()).await?;
                let mut b = [0u8; 1];
                let n = s.read(&mut b).await?;
                Ok::<_, io::Error>((n > 0).then(|| t.elapsed().as_millis() as u32))
            };
            timeout(Duration::from_secs(4), second).await.ok().and_then(|r| r.ok()).flatten()
        }
        None => None,
    };
    Ok((ms, rtt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RuleKind;

    #[test]
    fn probe_heads() {
        let h = parse_head(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nDate: x\r\n\r\n").unwrap();
        assert_eq!(h, HeadInfo { status: 204, content_length: Some(0), close: false });
        let h = parse_head(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 12\r\n\r\n").unwrap();
        assert!(h.close);
        assert_eq!(h.content_length, Some(12));
        let h = parse_head(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        assert!(h.close);
        assert!(parse_head(b"garbage").is_none());
    }

    fn rule(kind: RuleKind, v: &str, t: &str) -> Rule {
        Rule { kind, value: v.into(), target: t.into(), enabled: true }
    }

    fn table(strategy: DomainStrategy) -> Routing {
        let mut r = Routing::basic(
            vec![
                rule(RuleKind::DomainSuffix, "example.de", "direct"),
                rule(RuleKind::DomainKeyword, "video", "node:b"),
                rule(RuleKind::Port, "25", "block"),
                rule(RuleKind::IpCidr, "192.0.2.0/24", "direct"),
            ],
            "proxy",
        );
        r.upstreams.insert("a".into(), Upstream { port: 1, name: "A".into(), engine: EngineKind::Xray, udp: true });
        r.upstreams.insert("b".into(), Upstream { port: 2, name: "B".into(), engine: EngineKind::Singbox, udp: true });
        r.selected = Some("a".into());
        r.strategy = strategy;
        r
    }

    fn done(v: Verdict) -> (Decision, String) {
        match v {
            Verdict::Done(d, r) => (d, r),
            Verdict::NeedIp => panic!("unexpected NeedIp"),
        }
    }

    #[test]
    fn decide() {
        let r = table(DomainStrategy::AsIs);
        assert!(matches!(done(r.decide("Shop.Example.DE", 443, None, None)).0, Decision::Direct));
        assert!(matches!(done(r.decide("www.video.com", 443, None, None)).0, Decision::Via(ref u) if u.name == "B"));
        assert!(matches!(done(r.decide("smtp.x.com", 25, None, None)).0, Decision::Block));
        assert!(matches!(done(r.decide("192.0.2.9", 443, None, None)).0, Decision::Direct));
        let (d, rule) = done(r.decide("example.com", 443, None, None));
        assert!(matches!(d, Decision::Via(ref u) if u.name == "A"));
        assert_eq!(rule, "final");
    }

    #[test]
    fn ip_if_non_match() {
        let r = table(DomainStrategy::IpIfNonMatch);
        assert!(matches!(r.decide("example.com", 443, None, None), Verdict::NeedIp));
        let (d, rule) = done(r.decide("example.com", 443, "192.0.2.10".parse().ok(), None));
        assert!(matches!(d, Decision::Direct));
        assert!(rule.starts_with("#4"));
        // literal IPs never need resolving
        assert!(matches!(r.decide("203.0.113.1", 443, None, None), Verdict::Done(..)));
    }
}
