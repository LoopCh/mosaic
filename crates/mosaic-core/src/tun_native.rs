//! Mosaic's own TUN stack (feature `native-tun`).
//!
//! TUN device ─▶ ipstack (userspace TCP/IP) ─▶ hub routing
//!   * TCP flows: destination from the fake-IP table, else TLS SNI / HTTP Host
//!     sniffing, else the IP itself → `hub::connect_target` → relay
//!   * UDP flows: port 53 answered by the built-in fake-IP DNS, everything else
//!     → `udp::relay_flow` (same rules as TCP)
//!
//! Devices:
//!   * Windows — a wintun adapter; loop prevention by /32 bypass routes for node
//!     servers and pinning the hub's direct sockets to the physical interface.
//!   * Android (any unix with a ready TUN descriptor) — the interface created by
//!     VpnService. Mosaic's own app is excluded from the VPN there, so its direct
//!     sockets and the engines never loop back into the tunnel.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use crate::hub::SharedRouting;
use crate::resolver::Resolver;
use crate::stats::Stats;
use crate::util::LogBuf;

pub struct NativeParams {
    pub server_domains: Vec<String>,
    pub server_ips: Vec<IpAddr>,
    pub resolver: Arc<Resolver>,
    /// A TUN file descriptor prepared by the OS (Android VpnService). Unused on Windows.
    pub fd: Option<i32>,
    /// Linux: Mosaic's helper binary that creates and configures the interface.
    pub helper: Option<std::path::PathBuf>,
    /// Linux: the helper keeps the interface (and so blocks traffic) if Mosaic crashes.
    pub kill_switch: bool,
    pub routing: SharedRouting,
    pub stats: Arc<Stats>,
    pub logs: Arc<LogBuf>,
}

pub const TUN_NAME: &str = "Mosaic";
pub const TUN_ADDR: Ipv4Addr = Ipv4Addr::new(172, 19, 0, 1);
/// The DNS server address apps are given (answered by the stack itself).
pub const TUN_PEER: Ipv4Addr = Ipv4Addr::new(172, 19, 0, 2);

pub struct NativeTun {
    #[cfg(windows)]
    inner: win::Running,
    #[cfg(unix)]
    task: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    logs: Arc<LogBuf>,
    #[cfg(target_os = "linux")]
    helper: Option<linux_side::Helper>,
}

impl NativeTun {
    pub async fn start(p: NativeParams) -> Result<NativeTun, String> {
        #[cfg(windows)]
        {
            let inner = win::start(p).await?;
            Ok(NativeTun { inner })
        }
        #[cfg(unix)]
        {
            // Linux: the helper creates the interface and we attach to it.
            #[cfg(target_os = "linux")]
            let (fd, helper) = match p.fd {
                Some(fd) => (Some(fd), None),
                None => {
                    let (fd, h) = linux_side::start(&p).await?;
                    (Some(fd), Some(h))
                }
            };
            #[cfg(not(target_os = "linux"))]
            let fd = p.fd;
            let fd = fd.ok_or("the native TUN stack needs a TUN interface prepared by the system (Android VpnService)")?;
            let dev = fdtun::FdDev::new(fd).map_err(|e| format!("TUN descriptor: {e}"))?;
            let logs = p.logs.clone();
            let task = stack::spawn(dev, p.routing, p.stats, HashMap::new(), p.logs);
            logs.push("[tun] native TUN is up");
            Ok(NativeTun {
                task,
                logs,
                #[cfg(target_os = "linux")]
                helper,
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = p;
            Err("the native TUN stack is not available on this OS".into())
        }
    }

    /// `hold`: Linux — leave the interface and its routes in place (a reconnect is coming,
    /// or the kill switch must keep blocking); the next run takes them over.
    pub async fn stop(self, hold: bool) {
        let _ = hold;
        #[cfg(windows)]
        self.inner.stop().await;
        #[cfg(unix)]
        {
            // Dropping the stack closes the descriptor, which tears the VPN interface down
            // (Android); on Linux the helper removes the interface and its routes.
            self.task.abort();
            let _ = self.task.await;
            #[cfg(target_os = "linux")]
            if let Some(h) = self.helper {
                h.stop(hold).await;
            }
            self.logs.push("[tun] native TUN stopped");
        }
    }
}

// ---------------------------------------------------------------- the stack (all platforms)

mod stack {
    use super::*;
    use std::io;
    use std::net::SocketAddr;
    use std::time::Duration;

    use ipstack::{IpStack, IpStackConfig, IpStackStream, TcpConfig, TcpOptions};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    use crate::dns::{self, FakeIpPool};
    use crate::hub::{self, current};

    pub const MTU: u16 = 1500;
    /// MSS announced to apps in the SYN-ACK. Without it apps fall back to 536 bytes and
    /// cut every request into small pieces, which some servers do not accept (see
    /// `complete_tls_record`). 1440 fits IPv4 and IPv6 headers into the 1500 MTU.
    const MSS: u16 = 1440;
    /// Receive buffer / unacknowledged data per TCP flow (the stack has no window
    /// scaling, so 64 KiB is the most a window can say).
    const TCP_WINDOW: usize = 65535;
    const SNIFF_PORTS: [u16; 4] = [80, 443, 8080, 8443];

    /// State shared by all flows: fake-IP table and real answers for node servers.
    struct DnsState {
        fake: Mutex<FakeIpPool>,
        real: HashMap<String, Vec<IpAddr>>,
    }

    /// Packet and flow counters, logged every few seconds while traffic changes —
    /// shows at a glance whether packets reach the stack, flows are accepted and replies go out.
    #[derive(Default)]
    pub struct Counters {
        pub pkts_in: std::sync::atomic::AtomicU64,
        pub pkts_out: std::sync::atomic::AtomicU64,
        tcp: std::sync::atomic::AtomicU64,
        udp: std::sync::atomic::AtomicU64,
        dns: std::sync::atomic::AtomicU64,
        errors: std::sync::atomic::AtomicU64,
    }

    /// The device with packet counting.
    struct Counted<D> {
        dev: D,
        c: Arc<Counters>,
    }

    impl<D: AsyncRead + Unpin> AsyncRead for Counted<D> {
        fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<io::Result<()>> {
            let before = buf.filled().len();
            let r = std::pin::Pin::new(&mut self.dev).poll_read(cx, buf);
            if let std::task::Poll::Ready(Ok(())) = &r {
                if buf.filled().len() > before {
                    self.c.pkts_in.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            r
        }
    }

    impl<D: AsyncWrite + Unpin> AsyncWrite for Counted<D> {
        fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, data: &[u8]) -> std::task::Poll<io::Result<usize>> {
            let r = std::pin::Pin::new(&mut self.dev).poll_write(cx, data);
            if let std::task::Poll::Ready(Ok(_)) = &r {
                self.c.pkts_out.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            r
        }
        fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.dev).poll_flush(cx)
        }
        fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.dev).poll_shutdown(cx)
        }
    }

    /// Run the userspace stack on `dev` until the task is aborted or the device fails.
    pub fn spawn<D>(dev: D, routing: SharedRouting, stats: Arc<Stats>, real: HashMap<String, Vec<IpAddr>>, logs: Arc<LogBuf>) -> JoinHandle<()>
    where
        D: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        use std::sync::atomic::Ordering::Relaxed;
        let c = Arc::new(Counters::default());
        let mut cfg = IpStackConfig::default();
        cfg.mtu_unchecked(MTU);
        let mut tcp_cfg = TcpConfig::default();
        tcp_cfg.options = Some(vec![TcpOptions::MaximumSegmentSize(MSS)]);
        tcp_cfg.read_buffer_size = TCP_WINDOW;
        tcp_cfg.max_unacked_bytes = TCP_WINDOW as u32;
        cfg.with_tcp_config(tcp_cfg);
        let mut stack = IpStack::new(cfg, Counted { dev, c: c.clone() });
        let dns_state = Arc::new(DnsState { fake: Mutex::new(FakeIpPool::default()), real });
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            let mut last = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
            loop {
                let accepted = tokio::select! {
                    a = stack.accept() => a,
                    _ = tick.tick() => {
                        let now = (c.pkts_in.load(Relaxed), c.pkts_out.load(Relaxed), c.tcp.load(Relaxed), c.udp.load(Relaxed), c.dns.load(Relaxed), c.errors.load(Relaxed));
                        if now != last {
                            logs.push(format!(
                                "[tun] packets in {} / out {} · tcp flows {} · udp flows {} · dns {} · errors {}",
                                now.0, now.1, now.2, now.3, now.4, now.5
                            ));
                            last = now;
                        }
                        continue;
                    }
                };
                match accepted {
                    Ok(IpStackStream::Tcp(tcp)) => {
                        c.tcp.fetch_add(1, Relaxed);
                        let (src, dst) = (tcp.local_addr(), tcp.peer_addr());
                        let (r, s, d, c2, l) = (routing.clone(), stats.clone(), dns_state.clone(), c.clone(), logs.clone());
                        tokio::spawn(async move {
                            if let Err(e) = handle_tcp(tcp, src, dst, r, s, d).await {
                                // Idle flows closed by the stack are not errors.
                                if e.kind() == std::io::ErrorKind::TimedOut {
                                    return;
                                }
                                if c2.errors.fetch_add(1, Relaxed) < 20 {
                                    l.push(format!("[tun] tcp {src} → {dst}: {e}"));
                                }
                            }
                        });
                    }
                    Ok(IpStackStream::Udp(udp)) => {
                        let (src, dst) = (udp.local_addr(), udp.peer_addr());
                        if dst.port() == 53 { c.dns.fetch_add(1, Relaxed); } else { c.udp.fetch_add(1, Relaxed); }
                        let (r, s, d, c2, l) = (routing.clone(), stats.clone(), dns_state.clone(), c.clone(), logs.clone());
                        tokio::spawn(async move {
                            if let Err(e) = handle_udp(udp, src, dst, r, s, d).await {
                                // Idle flows closed by the stack are not errors.
                                if e.kind() == std::io::ErrorKind::TimedOut {
                                    return;
                                }
                                if c2.errors.fetch_add(1, Relaxed) < 20 {
                                    l.push(format!("[tun] udp {src} → {dst}: {e}"));
                                }
                            }
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        logs.push(format!("[tun] stack stopped: {e:?}"));
                        break;
                    }
                }
            }
        })
    }

    fn target_host(dst: IpAddr, dns: &DnsState) -> Option<String> {
        if let IpAddr::V4(v4) = dst {
            let pool = dns.fake.lock().unwrap();
            if pool.contains(v4) {
                return pool.lookup(v4).map(|s| s.to_string());
            }
        }
        Some(dst.to_string())
    }

    /// Application name for a flow from `src` (journal + per-app rules).
    fn flow_process(src: SocketAddr, dst: SocketAddr, proto: crate::procinfo::Proto, _routing: &SharedRouting) -> Option<String> {
        crate::procinfo::flow_owner(src, dst, proto)
    }

    /// If `buf` starts a TLS record (a ClientHello), read until the whole record is
    /// there. Apps send it in several TCP segments; forwarding the pieces one by one
    /// makes some servers and middleboxes (SNI routers that look only at the first
    /// packet) drop the connection — and the SNI may sit beyond the first piece.
    /// Bounded: 500 ms and 20 KB.
    async fn complete_tls_record<T: AsyncRead + Unpin>(tcp: &mut T, buf: &mut Vec<u8>) {
        if buf.len() < 5 || buf[0] != 0x16 || buf[1] != 0x03 {
            return;
        }
        let need = 5 + u16::from_be_bytes([buf[3], buf[4]]) as usize;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        let mut chunk = vec![0u8; 4096];
        while buf.len() < need && buf.len() < 20_000 {
            match tokio::time::timeout_at(deadline, tcp.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                _ => break,
            }
        }
    }

    async fn handle_tcp<T>(
        mut tcp: T,
        src: SocketAddr,
        dst: SocketAddr,
        routing: SharedRouting,
        stats: Arc<Stats>,
        dns: Arc<DnsState>,
    ) -> io::Result<()>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        // A fake IP we no longer know (e.g. after a restart): refuse, the app will re-resolve.
        let Some(mut host) = target_host(dst.ip(), &dns) else { return Ok(()) };
        let mut first = Vec::new();
        if host.parse::<IpAddr>().is_ok() && SNIFF_PORTS.contains(&dst.port()) {
            let mut buf = vec![0u8; 4096];
            if let Ok(Ok(n)) = timeout(Duration::from_millis(300), tcp.read(&mut buf)).await {
                buf.truncate(n);
                complete_tls_record(&mut tcp, &mut buf).await;
                if let Some(h) = crate::sniff::sniff(&buf) {
                    host = h;
                }
                first = buf;
            }
        }
        let process = flow_process(src, dst, crate::procinfo::Proto::Tcp, &routing);
        let r = current(&routing);
        let (mut up, conn) = hub::connect_target(&host, dst.port(), process.as_deref(), &r, &stats).await?;
        if first.is_empty() {
            // Whoever speaks first: if it is the app (TLS, HTTP…), take its first message
            // whole; if the server talks first (SMTP, FTP…), go straight to relaying.
            let mut buf = vec![0u8; 4096];
            let got = tokio::select! {
                r = tcp.read(&mut buf) => Some(r),
                _ = up.readable() => None,
            };
            match got {
                Some(Ok(n)) => {
                    buf.truncate(n);
                    complete_tls_record(&mut tcp, &mut buf).await;
                    first = buf;
                }
                Some(Err(e)) => {
                    stats.close(&conn, Some(e.to_string()));
                    return Err(e);
                }
                None => {}
            }
        }
        if !first.is_empty() {
            // One write: the kernel cuts it into full-size segments, as if the app
            // had connected directly.
            if let Err(e) = up.write_all(&first).await {
                stats.close(&conn, Some(e.to_string()));
                return Err(e);
            }
            stats.add(&conn, first.len() as u64, true);
        }
        hub::relay(tcp, up, conn, stats).await
    }

    async fn handle_udp<U>(
        mut udp: U,
        src: SocketAddr,
        dst: SocketAddr,
        routing: SharedRouting,
        stats: Arc<Stats>,
        dns: Arc<DnsState>,
    ) -> io::Result<()>
    where
        U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if dst.port() == 53 {
            return serve_dns(&mut udp, &dns, &routing).await;
        }
        // Multicast / broadcast (mDNS, LLMNR, SSDP…) is local-network chatter that has
        // nowhere to go through a proxy: drop it quietly.
        let local_only = match dst.ip() {
            IpAddr::V4(v4) => v4.is_multicast() || v4.is_broadcast() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_multicast() || (v6.segments()[0] & 0xffc0) == 0xfe80,
        };
        if local_only {
            return Ok(());
        }
        let Some(host) = target_host(dst.ip(), &dns) else { return Ok(()) };
        let process = flow_process(src, dst, crate::procinfo::Proto::Udp, &routing);
        crate::udp::relay_flow(udp, host, dst.port(), process, current(&routing), stats).await
    }

    /// Fake-IP DNS:
    ///   * A → fake address (real ones for node servers)
    ///   * AAAA, HTTPS/SVCB → empty (they would hand apps real IPs and bypass fake-IP routing)
    ///   * everything else (MX, TXT, SRV, CAA, PTR …) → forwarded through the tunnel
    async fn serve_dns<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, st: &DnsState, routing: &SharedRouting) -> io::Result<()> {
        let mut buf = vec![0u8; 1500];
        loop {
            let n = match timeout(Duration::from_secs(30), s.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => n,
                _ => return Ok(()),
            };
            let q = &buf[..n];
            let Some((_, question, end)) = dns::parse_query(q) else { continue };
            let resp = if let Some(ips) = st.real.get(&question.name) {
                dns::build_response(q, end, question.qtype, ips, 60, 0)
            } else if question.qtype == dns::TYPE_A {
                let ip = st.fake.lock().unwrap().alloc(&question.name);
                dns::build_response(q, end, dns::TYPE_A, &[IpAddr::V4(ip)], 1, 0)
            } else if matches!(question.qtype, dns::TYPE_AAAA | dns::TYPE_SVCB | dns::TYPE_HTTPS) {
                dns::build_response(q, end, question.qtype, &[], 1, 0)
            } else {
                let r = current(routing);
                match r.remote_exchange(q).await {
                    Ok(resp) if resp.len() >= 12 => resp,
                    _ => dns::build_response(q, end, question.qtype, &[], 1, 2), // SERVFAIL
                }
            };
            s.write_all(&resp).await?;
        }
    }
}

// ---------------------------------------------------------------- Windows (wintun)

#[cfg(windows)]
mod win {
    use super::*;
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::pin::Pin;
    use std::process::Command;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use super::stack::{self, MTU};

    pub struct Running {
        task: JoinHandle<()>,
        session: Arc<wintun::Session>,
        _adapter: Arc<wintun::Adapter>,
        bypass: Vec<(String, u32)>,
        logs: Arc<LogBuf>,
    }

    // ------------------------------------------------------------ device

    struct WinDev {
        session: Arc<wintun::Session>,
        rx: mpsc::UnboundedReceiver<Vec<u8>>,
    }

    impl AsyncRead for WinDev {
        fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(pkt)) => {
                    let n = pkt.len().min(buf.remaining());
                    buf.put_slice(&pkt[..n]);
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(None) => Poll::Ready(Ok(())),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    impl AsyncWrite for WinDev {
        fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
            match self.session.allocate_send_packet(buf.len() as u16) {
                Ok(mut p) => {
                    p.bytes_mut().copy_from_slice(buf);
                    self.session.send_packet(p);
                    Poll::Ready(Ok(buf.len()))
                }
                Err(e) => Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, format!("wintun send: {e:?}")))),
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    // ------------------------------------------------------------ OS plumbing

    fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new(cmd)
            .args(args)
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("{cmd}: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(text)
        } else {
            Err(format!("{cmd} {}: {}{}", args.join(" "), text.trim(), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    fn netsh(args: &[&str]) -> Result<String, String> {
        run("netsh", args)
    }

    /// (gateway, interface index) of the best physical IPv4 default route.
    fn physical_default_route() -> Result<(String, u32), String> {
        let script = format!(
            "Get-NetRoute -AddressFamily IPv4 -DestinationPrefix 0.0.0.0/0 -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.InterfaceAlias -ne '{TUN_NAME}' -and $_.NextHop -ne '0.0.0.0' }} | \
             Sort-Object RouteMetric | Select-Object -First 1 | \
             ForEach-Object {{ \"$($_.NextHop) $($_.ifIndex)\" }}"
        );
        let out = run("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script])?;
        let line = out.lines().map(str::trim).find(|l| !l.is_empty()).ok_or("no IPv4 default route found")?;
        let (gw, idx) = line.split_once(' ').ok_or_else(|| format!("unexpected route output: {line}"))?;
        let idx: u32 = idx.trim().parse().map_err(|_| format!("unexpected route output: {line}"))?;
        Ok((gw.to_string(), idx))
    }

    fn load_wintun() -> Result<wintun::Wintun, String> {
        let found = crate::engines::search_dirs().into_iter().map(|d| d.join("wintun.dll")).find(|p| p.is_file());
        let res = match &found {
            Some(p) => unsafe { wintun::load_from_path(p) },
            None => unsafe { wintun::load() },
        };
        res.map_err(|e| format!("cannot load wintun.dll ({e:?}); put it into the engines folder"))
    }

    // ------------------------------------------------------------ start / stop

    pub async fn start(p: NativeParams) -> Result<Running, String> {
        let logs = p.logs.clone();
        let (gw, phys_idx) = physical_default_route()?;
        logs.push(format!("[tun] physical gateway {gw} (interface {phys_idx})"));

        // Resolve node servers with real DNS before the routes change.
        let mut real: HashMap<String, Vec<IpAddr>> = HashMap::new();
        let mut bypass_ips: Vec<IpAddr> = p.server_ips.clone();
        for d in &p.server_domains {
            match p.resolver.resolve(d).await {
                Ok(ips) => {
                    bypass_ips.extend(ips.iter().copied());
                    real.insert(d.clone(), ips);
                }
                Err(e) => logs.push(format!("[tun] cannot resolve node server {d}: {e}")),
            }
        }
        if let Some(dns) = p.resolver.server() {
            bypass_ips.push(dns.ip());
        }
        bypass_ips.sort();
        bypass_ips.dedup();

        let wt = load_wintun()?;
        let adapter = wintun::Adapter::create(&wt, TUN_NAME, TUN_NAME, None).map_err(|e| format!("cannot create the TUN adapter: {e:?}"))?;
        let session = Arc::new(adapter.start_session(wintun::MAX_RING_CAPACITY).map_err(|e| format!("wintun session: {e:?}"))?);

        let mut running = Running { task: tokio::spawn(async {}), session: session.clone(), _adapter: adapter, bypass: Vec::new(), logs: logs.clone() };
        if let Err(e) = configure(&mut running, &gw, phys_idx, &bypass_ips) {
            running.stop().await;
            return Err(e);
        }
        crate::net::set_bypass_interface(phys_idx);

        // Packets: wintun (blocking) → channel → ipstack.
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let reader = session.clone();
        std::thread::spawn(move || {
            while let Ok(pkt) = reader.receive_blocking() {
                if tx.send(pkt.bytes().to_vec()).is_err() {
                    break;
                }
            }
        });
        let dev = WinDev { session: session.clone(), rx };
        running.task = stack::spawn(dev, p.routing.clone(), p.stats.clone(), real, logs.clone());
        logs.push(format!("[tun] native TUN '{TUN_NAME}' is up ({} bypass routes)", running.bypass.len()));
        Ok(running)
    }

    fn configure(r: &mut Running, gw: &str, phys_idx: u32, bypass_ips: &[IpAddr]) -> Result<(), String> {
        let name = format!("name={TUN_NAME}");
        netsh(&["interface", "ipv4", "set", "address", &name, "static", &TUN_ADDR.to_string(), "255.255.255.252"])?;
        let _ = netsh(&["interface", "ipv4", "set", "subinterface", TUN_NAME, &format!("mtu={MTU}"), "store=active"]);
        if let Err(e) = netsh(&["interface", "ipv4", "set", "interface", TUN_NAME, "metric=1"]) {
            r.logs.push(format!("[tun] interface metric: {e}"));
        }
        netsh(&["interface", "ipv4", "set", "dnsservers", &name, "source=static", &format!("address={TUN_PEER}"), "register=none", "validate=no"])?;
        let idx = phys_idx.to_string();
        for ip in bypass_ips {
            let IpAddr::V4(v4) = ip else { continue };
            let prefix = format!("{v4}/32");
            match netsh(&["interface", "ipv4", "add", "route", &prefix, &idx, gw, "metric=1", "store=active"]) {
                Ok(_) => r.bypass.push((prefix, phys_idx)),
                Err(e) => r.logs.push(format!("[tun] bypass route {prefix}: {e}")),
            }
        }
        let peer = TUN_PEER.to_string();
        for half in ["0.0.0.0/1", "128.0.0.0/1"] {
            netsh(&["interface", "ipv4", "add", "route", half, TUN_NAME, &peer, "metric=1", "store=active"])?;
        }
        // A default route on the TUN as well (the halves above already win by prefix length):
        // Windows asks the DNS servers of the interface with the best *default* route first —
        // without it the system DNS may keep using the physical adapter, whose DNS is blocked.
        if let Err(e) = netsh(&["interface", "ipv4", "add", "route", "0.0.0.0/0", TUN_NAME, &peer, "metric=1", "store=active"]) {
            r.logs.push(format!("[tun] default route: {e}"));
        }
        // Answers cached before the tunnel came up would skip it.
        let _ = run("ipconfig", &["/flushdns"]);
        // IPv6: capture it too so it cannot bypass the tunnel (DNS answers are IPv4-only).
        let _ = netsh(&["interface", "ipv6", "set", "address", TUN_NAME, "fdfe:dcba:9876::1/126", "store=active"]);
        for half in ["::/1", "8000::/1"] {
            if let Err(e) = netsh(&["interface", "ipv6", "add", "route", half, TUN_NAME, "fdfe:dcba:9876::2", "metric=1", "store=active"]) {
                r.logs.push(format!("[tun] IPv6 route {half}: {e}"));
            }
        }
        Ok(())
    }

    impl Running {
        pub async fn stop(self) {
            self.task.abort();
            crate::net::set_bypass_interface(0);
            let _ = self.session.shutdown();
            for (prefix, idx) in &self.bypass {
                let _ = netsh(&["interface", "ipv4", "delete", "route", prefix, &idx.to_string()]);
            }
            // Routes on the TUN interface disappear with the adapter.
            self.logs.push("[tun] native TUN stopped");
        }
    }
}

// ---------------------------------------------------------------- Linux: helper side

#[cfg(target_os = "linux")]
mod linux_side {
    use super::*;
    use crate::linux_tun::{self, HELPER_ARG, TUN_NAME};
    use std::os::fd::IntoRawFd;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::process::{Child, ChildStdin, ChildStdout, Command};

    pub struct Helper {
        child: Child,
        stdin: Option<ChildStdin>,
    }

    impl Helper {
        pub async fn stop(mut self, hold: bool) {
            if let Some(mut stdin) = self.stdin.take() {
                let _ = stdin.write_all(if hold { b"hold\n" } else { b"stop\n" }).await;
                drop(stdin);
            }
            if !hold {
                // While the interface stays, Mosaic's own sockets must keep going around it.
                crate::net::set_bypass_interface(0);
                crate::net::set_bypass_device("");
            }
            if tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await.is_err() {
                let _ = self.child.kill().await;
            }
        }
    }

    async fn expect(lines: &mut Lines<BufReader<ChildStdout>>, want: &str, logs: &LogBuf) -> Result<(), String> {
        loop {
            let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
                .await
                .map_err(|_| format!("the TUN helper did not answer ({want})"))?
                .map_err(|e| e.to_string())?
                .ok_or("the TUN helper exited")?;
            if line == want {
                return Ok(());
            }
            if let Some(e) = line.strip_prefix("error: ") {
                return Err(format!("TUN: {e}"));
            }
            logs.push(format!("[tun] {line}"));
        }
    }

    pub async fn start(p: &NativeParams) -> Result<(i32, Helper), String> {
        let exe = p.helper.clone().ok_or("TUN helper missing")?;
        let (phys, gw) = linux_tun::physical_default_route().ok_or("no IPv4 default route (is the network connected?)")?;
        let bypass: Vec<String> = p.server_ips.iter().filter(|ip| ip.is_ipv4()).map(|ip| ip.to_string()).collect();
        let dns: Vec<String> = linux_tun::resolv_nameservers().iter().map(|ip| ip.to_string()).filter(|ip| !bypass.contains(ip)).collect();
        let uid = unsafe { libc::getuid() }.to_string();
        let join = |v: &[String]| if v.is_empty() { "-".to_string() } else { v.join(",") };
        p.logs.push(format!("[tun] physical interface {phys} (gateway {})", gw.map(|g| g.to_string()).unwrap_or_else(|| "-".into())));
        let mut child = Command::new(&exe)
            .arg(HELPER_ARG)
            .arg(&uid)
            .arg(&phys)
            .arg(gw.map(|g| g.to_string()).unwrap_or_else(|| "-".into()))
            .arg(join(&bypass))
            .arg(join(&dns))
            .arg("1")
            .arg(if p.kill_switch { "ks" } else { "-" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start the TUN helper: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("helper stdin")?;
        let mut lines = BufReader::new(child.stdout.take().ok_or("helper stdout")?).lines();
        let res: Result<i32, String> = async {
            expect(&mut lines, "created", &p.logs).await?;
            let fd = linux_tun::open_tun(TUN_NAME).map_err(|e| format!("cannot attach to the TUN interface: {e}"))?;
            stdin.write_all(b"go\n").await.map_err(|e| e.to_string())?;
            expect(&mut lines, "ready", &p.logs).await?;
            Ok(fd.into_raw_fd())
        }
        .await;
        match res {
            Ok(fd) => {
                // Later helper messages (warnings) go to the log.
                let logs = p.logs.clone();
                tokio::spawn(async move {
                    while let Ok(Some(l)) = lines.next_line().await {
                        logs.push(format!("[tun] {l}"));
                    }
                });
                crate::net::set_bypass_device(&phys);
                crate::net::set_bypass_interface(linux_tun::ifindex(&phys).unwrap_or(1));
                p.logs.push(format!("[tun] interface '{TUN_NAME}' is up ({} bypass routes)", bypass.len()));
                Ok((fd, Helper { child, stdin: Some(stdin) }))
            }
            Err(e) => {
                drop(stdin);
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------- unix TUN descriptor

#[cfg(unix)]
mod fdtun {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::pin::Pin;
    use std::task::{ready, Context, Poll};

    use tokio::io::unix::AsyncFd;
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    /// A TUN descriptor without packet-info headers: one read = one IP packet.
    pub struct FdDev {
        fd: AsyncFd<OwnedFd>,
    }

    impl FdDev {
        pub fn new(raw: RawFd) -> io::Result<Self> {
            // SAFETY: the caller hands over ownership of a valid descriptor.
            let owned = unsafe { OwnedFd::from_raw_fd(raw) };
            let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            // `new` is deprecated in recent tokio; the descriptor is owned (OwnedFd), so it is sound here.
            #[allow(deprecated)]
            let fd = AsyncFd::new(owned)?;
            Ok(Self { fd })
        }
    }

    impl AsyncRead for FdDev {
        fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            loop {
                let mut guard = ready!(self.fd.poll_read_ready(cx))?;
                let unfilled = buf.initialize_unfilled();
                let res = guard.try_io(|inner| {
                    let n = unsafe { libc::read(inner.as_raw_fd(), unfilled.as_mut_ptr() as *mut libc::c_void, unfilled.len()) };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                });
                match res {
                    Ok(Ok(n)) => {
                        buf.advance(n);
                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(e)) => return Poll::Ready(Err(e)),
                    Err(_would_block) => continue,
                }
            }
        }
    }

    impl AsyncWrite for FdDev {
        fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
            loop {
                let mut guard = ready!(self.fd.poll_write_ready(cx))?;
                let res = guard.try_io(|inner| {
                    let n = unsafe { libc::write(inner.as_raw_fd(), data.as_ptr() as *const libc::c_void, data.len()) };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                });
                match res {
                    Ok(r) => return Poll::Ready(r),
                    Err(_would_block) => continue,
                }
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
}
