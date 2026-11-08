//! The hub data plane: one mixed inbound (SOCKS5 + HTTP proxy) on loopback,
//! a router that evaluates rules, and a dispatcher that connects either
//! directly or through the local SOCKS inbound of the engine serving a node.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::model::{EngineKind, Rule};
use crate::rules;
use crate::stats::{ConnEntry, Stats};
use crate::util::LogBuf;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const BUF: usize = 16 * 1024;

#[derive(Clone, Debug)]
pub struct Upstream {
    pub port: u16,
    pub name: String,
    pub engine: EngineKind,
}

#[derive(Clone, Debug)]
pub enum Decision {
    Direct,
    Block,
    Via(Upstream),
}

/// Immutable routing table; swapped atomically on every change.
#[derive(Clone, Debug, Default)]
pub struct Routing {
    pub rules: Vec<Rule>,
    pub final_target: String,
    pub selected: Option<String>,
    pub upstreams: HashMap<String, Upstream>,
    /// TUN mode: "direct" goes through this SOCKS port of the TUN adapter,
    /// because the system resolver would hand the hub fake IPs.
    pub direct_via: Option<u16>,
}

impl Routing {
    pub fn decide(&self, host: &str, port: u16) -> (Decision, String) {
        let h = host.to_ascii_lowercase();
        let ip: Option<IpAddr> = h.parse().ok();
        for (i, r) in self.rules.iter().enumerate() {
            if r.enabled && rules::matches(r, &h, ip, port) {
                let mut v = r.value.clone();
                if v.chars().count() > 40 {
                    v = v.chars().take(40).collect::<String>() + "…";
                }
                return (self.target(&r.target), format!("#{} {} {}", i + 1, r.kind.label(), v));
            }
        }
        (self.target(&self.final_target), "final".into())
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

pub struct Hub {
    pub port: u16,
    task: JoinHandle<()>,
    routing: Arc<RwLock<Arc<Routing>>>,
}

impl Hub {
    pub async fn start(port: u16, routing: Routing, stats: Arc<Stats>, logs: Arc<LogBuf>) -> Result<Hub, String> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("порт 127.0.0.1:{port} занят или недоступен: {e}"))?;
        let routing = Arc::new(RwLock::new(Arc::new(routing)));
        let shared = routing.clone();
        logs.push(format!("[hub] слушаю 127.0.0.1:{port} (SOCKS5 + HTTP)"));
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((sock, _)) => {
                        let r = shared.read().unwrap().clone();
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            let _ = handle(sock, r, stats).await;
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

    pub fn stop(self) {
        self.task.abort();
    }
}

fn other(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg.into())
}

async fn handle(mut s: TcpStream, routing: Arc<Routing>, stats: Arc<Stats>) -> io::Result<()> {
    let _ = s.set_nodelay(true);
    let first = s.read_u8().await?;
    if first == 5 {
        socks5(s, &routing, &stats).await
    } else {
        http(s, first, &routing, &stats).await
    }
}

// ---------------------------------------------------------------- SOCKS5

async fn socks5(mut s: TcpStream, routing: &Routing, stats: &Arc<Stats>) -> io::Result<()> {
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
    if h[1] != 1 {
        // Only CONNECT for now; UDP ASSOCIATE comes with the native TUN stack.
        s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    }
    match open(&host, port, routing, stats).await {
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

async fn read_addr<R: AsyncRead + Unpin>(r: &mut R, atyp: u8) -> io::Result<(String, u16)> {
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
        _ => return Err(other("socks: неизвестный тип адреса")),
    };
    let port = r.read_u16().await?;
    Ok((host, port))
}

/// SOCKS5 CONNECT through a local engine inbound.
pub async fn socks_connect(proxy_port: u16, host: &str, port: u16) -> io::Result<TcpStream> {
    let mut s = TcpStream::connect(("127.0.0.1", proxy_port)).await?;
    let _ = s.set_nodelay(true);
    s.write_all(&[5, 1, 0]).await?;
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await?;
    if r != [5, 0] {
        return Err(other("socks: движок отклонил рукопожатие"));
    }
    let mut req = vec![5u8, 1, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            req.push(1);
            req.extend_from_slice(&a.octets());
        }
        Ok(IpAddr::V6(a)) => {
            req.push(4);
            req.extend_from_slice(&a.octets());
        }
        Err(_) => {
            let b = host.as_bytes();
            if b.len() > 255 {
                return Err(other("слишком длинное доменное имя"));
            }
            req.push(3);
            req.push(b.len() as u8);
            req.extend_from_slice(b);
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await?;
    let mut h = [0u8; 4];
    s.read_exact(&mut h).await?;
    if h[1] != 0 {
        return Err(other(format!("движок вернул ошибку SOCKS {}", h[1])));
    }
    read_addr(&mut s, h[3]).await?;
    Ok(s)
}

// ---------------------------------------------------------------- HTTP proxy

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn split_hp(s: &str, default_port: u16) -> io::Result<(String, u16)> {
    if let Some(r) = s.strip_prefix('[') {
        let (h, p) = r.split_once(']').ok_or_else(|| other("плохой адрес"))?;
        let port = p.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
        return Ok((h.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Ok((h.to_string(), p.parse().map_err(|_| other("плохой порт"))?)),
        _ => Ok((s.to_string(), default_port)),
    }
}

async fn http(mut s: TcpStream, first: u8, routing: &Routing, stats: &Arc<Stats>) -> io::Result<()> {
    let mut buf = vec![first];
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > 64 * 1024 {
            return Err(other("слишком большой HTTP-заголовок"));
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
        match open(&host, port, routing, stats).await {
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
        match open(&host, port, routing, stats).await {
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

async fn open(host: &str, port: u16, routing: &Routing, stats: &Arc<Stats>) -> io::Result<(TcpStream, Arc<ConnEntry>)> {
    let (decision, rule) = routing.decide(host, port);
    let (outbound, engine) = match &decision {
        Decision::Direct => ("direct".to_string(), None),
        Decision::Block => ("block".to_string(), None),
        Decision::Via(u) => (u.name.clone(), Some(u.engine.label().to_string())),
    };
    let conn = stats.open(host, port, rule, outbound, engine);
    let res = match decision {
        Decision::Block => Err(io::Error::new(io::ErrorKind::PermissionDenied, "заблокировано правилом")),
        Decision::Direct => {
            let fut = async {
                match routing.direct_via {
                    Some(p) => socks_connect(p, host, port).await,
                    None => TcpStream::connect((host, port)).await,
                }
            };
            match timeout(CONNECT_TIMEOUT, fut).await {
                Ok(r) => r,
                Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "таймаут соединения")),
            }
        }
        Decision::Via(u) => match timeout(CONNECT_TIMEOUT, socks_connect(u.port, host, port)).await {
            Ok(r) => r,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "таймаут через движок")),
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

async fn relay(client: TcpStream, up: TcpStream, conn: Arc<ConnEntry>, stats: Arc<Stats>) -> io::Result<()> {
    let (cr, cw) = client.into_split();
    let (ur, uw) = up.into_split();
    let res = tokio::try_join!(
        pipe(cr, uw, conn.clone(), stats.clone(), true),
        pipe(ur, cw, conn.clone(), stats.clone(), false)
    );
    let err = res.err().map(|e| e.to_string());
    stats.close(&conn, err);
    Ok(())
}

async fn pipe(mut r: OwnedReadHalf, mut w: OwnedWriteHalf, conn: Arc<ConnEntry>, stats: Arc<Stats>, upload: bool) -> io::Result<()> {
    let mut buf = vec![0u8; BUF];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            let _ = w.shutdown().await;
            return Ok(());
        }
        w.write_all(&buf[..n]).await?;
        stats.add(&conn, n as u64, upload);
    }
}

/// Latency test through an engine inbound: time from SOCKS CONNECT to the
/// first byte of the HTTP response (includes the proxy handshake).
pub async fn url_test(proxy_port: u16, url: &str) -> Result<u32, String> {
    let rest = url.strip_prefix("http://").ok_or("тест поддерживает только http:// URL")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_hp(authority, 80).map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let fut = async {
        let mut s = socks_connect(proxy_port, &host, port).await?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Mosaic\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await?;
        let mut b = [0u8; 1];
        let n = s.read(&mut b).await?;
        if n == 0 {
            return Err(other("сервер закрыл соединение"));
        }
        Ok::<(), io::Error>(())
    };
    match timeout(Duration::from_secs(8), fut).await {
        Ok(Ok(())) => Ok(started.elapsed().as_millis() as u32),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("таймаут".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RuleKind;

    #[test]
    fn decide() {
        let mut up = HashMap::new();
        up.insert("a".to_string(), Upstream { port: 1, name: "A".into(), engine: EngineKind::Xray });
        up.insert("b".to_string(), Upstream { port: 2, name: "B".into(), engine: EngineKind::Singbox });
        let r = Routing {
            rules: vec![
                Rule { kind: RuleKind::DomainSuffix, value: "ru".into(), target: "direct".into(), enabled: true },
                Rule { kind: RuleKind::DomainKeyword, value: "youtube".into(), target: "node:b".into(), enabled: true },
                Rule { kind: RuleKind::Port, value: "25".into(), target: "block".into(), enabled: true },
            ],
            final_target: "proxy".into(),
            selected: Some("a".into()),
            upstreams: up,
            direct_via: None,
        };
        assert!(matches!(r.decide("Ya.RU", 443).0, Decision::Direct));
        assert!(matches!(r.decide("www.youtube.com", 443).0, Decision::Via(ref u) if u.name == "B"));
        assert!(matches!(r.decide("smtp.x.com", 25).0, Decision::Block));
        let (d, rule) = r.decide("example.com", 443);
        assert!(matches!(d, Decision::Via(ref u) if u.name == "A"));
        assert_eq!(rule, "final");
    }
}
