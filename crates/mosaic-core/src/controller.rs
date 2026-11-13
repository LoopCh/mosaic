//! The controller owns state, the running session (hub + engines + TUN) and
//! is the only API the UI and CLI talk to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::engines::{self, singbox, Slot};
use crate::geo::{self, GeoData};
use crate::hub::{self, Hub, Routing, Upstream};
use crate::links;
use crate::model::*;
use crate::process::{self, EngineProcess};
use crate::resolver::{self, DnsCache, Resolver};
use crate::rules;
use crate::stats::{ConnView, Stats};
use crate::store;
use crate::sys;
use crate::updates::{self, AppUpdate, EngineUpdate};
use crate::util::{now_ms, LogBuf};

// ---------------------------------------------------------------- views for the UI

#[derive(Clone, Debug, Serialize)]
pub struct Latency {
    /// Connection time: first response byte on a new connection (handshakes included).
    pub ms: Option<u32>,
    /// One round trip through the tunnel on an open connection (like "ping" in speed tests).
    pub rtt: Option<u32>,
    pub error: Option<String>,
    pub at_ms: u64,
}

/// What a connect / reconnect is doing right now (polled by the UI without waiting
/// for the controller lock, which connect holds).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    /// prepare | engines | check | route | dns — None when idle.
    pub step: Option<&'static str>,
    pub detail: Option<String>,
    pub since_ms: u64,
}

/// Does traffic actually go through the active node?
#[derive(Clone, Debug, Default, Serialize)]
pub struct Health {
    /// "ok", "degraded" (the node stopped answering) or "unknown".
    pub state: &'static str,
    pub error: Option<String>,
    pub checked_ms: u64,
    pub ms: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct NodeView {
    #[serde(flatten)]
    pub node: Node,
    pub summary: String,
    pub supported: Vec<EngineKind>,
    pub effective: Option<EngineKind>,
    pub effective_error: Option<String>,
    pub latency: Option<Latency>,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub connected: bool,
    pub mode: Option<Mode>,
    pub tun_stack: Option<TunStack>,
    pub since_ms: Option<u64>,
    pub up: u64,
    pub down: u64,
    pub active_conns: usize,
    pub listen_port: Option<u16>,
    /// The local proxy accepts connections from the LAN.
    pub lan: bool,
    pub system_proxy: bool,
    pub tun: bool,
    /// DNS leak protection (WFP filters) is active.
    pub dns_protected: bool,
    pub health: Health,
    /// "off", "armed" (connected, protected) or "blocking" (not connected — traffic is blocked
    /// until Mosaic connects again or the user lifts the block).
    pub kill_switch: &'static str,
}

#[derive(Debug, Serialize)]
pub struct EngineInfo {
    pub kind: EngineKind,
    pub name: &'static str,
    pub path: Option<String>,
    pub running: bool,
    pub pids: Vec<u32>,
    pub nodes: usize,
}

#[derive(Debug, Serialize)]
pub struct EngineVersion {
    pub kind: EngineKind,
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    /// Mosaic version (kept in sync across all crates and tauri.conf.json).
    pub version: &'static str,
    pub nodes: Vec<NodeView>,
    pub subscriptions: Vec<Subscription>,
    pub rules: Vec<Rule>,
    pub settings: Settings,
    pub selected: Option<String>,
    pub status: Status,
    pub engines: Vec<EngineInfo>,
    pub is_admin: bool,
    pub data_dir: String,
    pub native_tun_available: bool,
    /// A newer Mosaic release, if found.
    pub app_update: Option<AppUpdate>,
    /// This computer's address in the local network (for "allow LAN").
    pub lan_ip: Option<String>,
    pub geoip_file: Option<String>,
    pub geosite_file: Option<String>,
    /// "windows", "android", "linux", "macos" — the UI hides what does not apply.
    pub platform: &'static str,
    /// Engines can be updated from the app on this platform.
    pub engine_updates: bool,
    /// Updated engines are installed (so "restore bundled versions" makes sense).
    pub engines_updated: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct ImportSummary {
    pub added: usize,
    pub updated: usize,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------- internals

struct Session {
    hub: Hub,
    engines: Vec<EngineProcess>,
    /// sing-box TUN adapter.
    front: Option<EngineProcess>,
    /// Per-app rules the running adapter was built with (rebuild only when they change).
    front_apps: Vec<(Vec<String>, singbox::FrontTarget)>,
    #[cfg(feature = "native-tun")]
    native: Option<crate::tun_native::NativeTun>,
    upstreams: HashMap<String, Upstream>,
    mode: Mode,
    tun_stack: TunStack,
    lan: bool,
    system_proxy: bool,
    started_ms: u64,
    /// sing-box adapter: SOCKS port used for the hub's "direct" traffic.
    direct_port: Option<u16>,
    /// Node server domains (real DNS answers inside the TUN adapter).
    server_domains: Vec<String>,
    /// Node server IPs (and the direct DNS server): routed around the TUN adapter, so the
    /// engines reach their servers directly even if process detection fails (Linux).
    server_ips: Vec<std::net::IpAddr>,
    geo: Arc<GeoData>,
    resolver: Arc<Resolver>,
    remote_cache: Arc<DnsCache>,
    logs: Arc<LogBuf>,
    #[cfg(windows)]
    dns_guard: Option<crate::wfp::DnsGuard>,
}

impl Session {
    fn dns_protected(&self) -> bool {
        #[cfg(windows)]
        {
            self.dns_guard.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    fn tun_up(&self) -> bool {
        #[cfg(feature = "native-tun")]
        {
            if self.native.is_some() {
                return true;
            }
        }
        self.front.is_some()
    }
}

struct Inner {
    state: State,
    session: Option<Session>,
    latency: HashMap<String, Latency>,
}

pub struct Controller {
    inner: Mutex<Inner>,
    pub stats: Arc<Stats>,
    pub logs: Arc<LogBuf>,
    /// Mirrors `settings.close_to_tray` for the window event handler.
    pub close_to_tray: AtomicBool,
    /// Ask the background loop for an immediate Smart Connect test.
    smart_kick: AtomicBool,
    /// Ask the background loop to check the active node at the next tick.
    check_kick: AtomicBool,
    app_update: std::sync::Mutex<Option<AppUpdate>>,
    /// Android: the VPN interface handed over by VpnService for the next TUN connect.
    tun_fd: std::sync::Mutex<Option<i32>>,
    progress: std::sync::Mutex<Progress>,
    health: std::sync::Mutex<Health>,
    /// The kill switch is in force (filters / held interface). Mirrored in `killswitch.armed`
    /// in the data folder, so a restart after a crash knows traffic is blocked.
    kill_armed: AtomicBool,
    dir: PathBuf,
    admin: bool,
}

/// Clears the connect progress when connect returns, however it returns.
struct ProgressDone<'a>(&'a Controller);

impl Drop for ProgressDone<'_> {
    fn drop(&mut self) {
        *self.0.progress.lock().unwrap() = Progress::default();
    }
}

/// Marker file: the kill switch is in force (survives a crash / restart).
const KILL_FLAG: &str = "killswitch.armed";

/// Where the kill switch works: TUN on Windows (WFP filters, both stacks) and on Linux with
/// Mosaic's own stack (the held interface). Android has it built in (system VPN settings).
fn kill_switch_supported(settings: &Settings) -> bool {
    settings.mode == Mode::Tun && (cfg!(windows) || (cfg!(target_os = "linux") && settings.tun_stack == TunStack::Native))
}

/// Linux, at start: what the kill switch left behind (if anything) and whether it still blocks.
/// A leftover interface without the kill switch (Mosaic was killed) only swallows traffic,
/// so it is removed right away.
#[cfg(target_os = "linux")]
fn kill_switch_startup_report(armed: bool) -> Vec<String> {
    let c = crate::linux_tun::route_check();
    let mut out = Vec::new();
    match (armed, c.iface) {
        (true, true) => {
            out.push("[killswitch] the last session did not end normally (Mosaic was closed or killed while connected): \
                      traffic stays blocked until you connect or press “Unblock”"
                .to_string());
            out.push(format!("[killswitch] state: {}", c.summary()));
            if c.leak().is_some() {
                out.push("[killswitch] WARNING: the blocking routes are not complete — traffic is NOT fully blocked".into());
                out.extend(crate::linux_tun::ip_snapshot().into_iter().map(|l| format!("[killswitch] {l}")));
            }
        }
        (true, false) => out.push(
            "[killswitch] WARNING: the kill switch was on, but its interface is gone (the computer restarted, or another program \
             removed it) — traffic is NOT blocked now"
                .into(),
        ),
        (false, true) => {
            out.push(format!("[tun] an interface 'Mosaic' is left from a session that was killed ({}) — removing it", c.summary()));
            let res = crate::linux_tun::helper_binary(&engines::bundled_copy_dir()).and_then(|exe| {
                let o = std::process::Command::new(&exe).arg("clean").output().map_err(|e| e.to_string())?;
                if o.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&o.stdout).trim().trim_start_matches("error: ").to_string())
                }
            });
            match res {
                Ok(()) => out.push("[tun] leftover interface removed".into()),
                Err(e) => out.push(format!("[tun] could not remove the leftover interface: {e}")),
            }
        }
        (false, false) => {}
    }
    out
}

/// The latency test / health check can open http:// and https:// URLs.
fn probe_url_ok(u: &str) -> bool {
    u.starts_with("http://") || u.starts_with("https://")
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn routing_for(st: &State, s: &Session) -> Routing {
    Routing {
        rules: st.rules.clone(),
        final_target: st.settings.final_target.clone(),
        selected: st.selected.clone(),
        upstreams: s.upstreams.clone(),
        direct_via: s.direct_port,
        geo: s.geo.clone(),
        strategy: st.settings.domain_strategy,
        resolver: s.resolver.clone(),
        remote_dns: resolver::parse_dns_server(&st.settings.remote_dns, "1.1.1.1:53"),
        ip_rule_dns: st.settings.ip_rule_dns,
        remote_cache: s.remote_cache.clone(),
        webrtc_guard: st.settings.webrtc_protection && s.mode == Mode::Tun,
        logs: Some(s.logs.clone()),
    }
}

/// Per-application rules translated for the sing-box TUN adapter.
fn app_rules(st: &State, s: &Session) -> Vec<(Vec<String>, singbox::FrontTarget)> {
    st.rules
        .iter()
        .filter(|r| r.enabled && r.kind == RuleKind::ProcessName)
        .map(|r| {
            let names: Vec<String> = r.value.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
            let target = match r.target.as_str() {
                "direct" => singbox::FrontTarget::Direct,
                "block" => singbox::FrontTarget::Block,
                "proxy" => singbox::FrontTarget::Hub,
                other => match other.strip_prefix("node:").and_then(|id| s.upstreams.get(id)) {
                    Some(u) => singbox::FrontTarget::Socks(u.port),
                    None => singbox::FrontTarget::Block,
                },
            };
            (names, target)
        })
        .collect()
}

fn server_domains(nodes: &[Node]) -> Vec<String> {
    let mut v: Vec<String> = nodes
        .iter()
        .filter(|n| n.server.parse::<std::net::IpAddr>().is_err())
        .map(|n| n.server.to_ascii_lowercase())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Address of the interface used for the default route (no packets are sent).
/// Windows: a Name Resolution Policy (NRPT) rule sending *all* names (".") to some DNS
/// server overrides the DNS of every interface, the TUN's included. Tailscale ("Use Tailscale
/// DNS settings", 100.100.100.100) and corporate VPNs set such rules. Mosaic must not override
/// or allow it (that server forwards outside the tunnel), so it explains instead.
#[cfg(windows)]
fn nrpt_override() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-DnsClientNrptPolicy | ForEach-Object { \"$($_.Namespace)|$($_.NameServers -join ',')\" }",
        ])
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    nrpt_hint(&text)
}

/// Lines "namespace|server,server" → explanation if all names are redirected.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn nrpt_hint(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some((ns, servers)) = line.trim().split_once('|') else { continue };
        if ns.trim() != "." || servers.trim().is_empty() {
            continue;
        }
        let servers = servers.trim();
        let who = if servers.contains("100.100.100.100") {
            "Tailscale (“Use Tailscale DNS settings”). Turn it off while using Mosaic TUN: Tailscale menu → \
             Preferences → uncheck “Use Tailscale DNS settings”, or run `tailscale set --accept-dns=false`."
                .to_string()
        } else {
            "another VPN or a corporate policy. Turn off its DNS setting or disconnect it while using Mosaic TUN.".to_string()
        };
        return Some(format!(
            "Windows sends all DNS queries to {servers} by a name resolution policy (NRPT) rule, bypassing \
             Mosaic's tunnel — that DNS would leak. The rule comes from {who}"
        ));
    }
    None
}

/// `n` with its server replaced by `ip`; the name stays in SNI / Host where it was implied.
fn pin_node(mut n: Node, ip: String) -> Node {
    let name = std::mem::replace(&mut n.server, ip);
    match &mut n.security {
        Security::Tls { sni, .. } | Security::Reality { sni, .. } if sni.is_empty() => *sni = name.clone(),
        _ => {}
    }
    match &mut n.transport {
        Transport::Ws { host, .. } | Transport::Httpupgrade { host, .. } | Transport::Xhttp { host, .. } if host.is_empty() => {
            *host = name.clone()
        }
        _ => {}
    }
    n
}

fn lan_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.to_string().starts_with("172.19.0.")).then(|| ip.to_string())
}

fn fix_selection(st: &mut State) {
    let ok = st.selected.as_ref().map_or(false, |sel| st.nodes.iter().any(|n| &n.id == sel));
    if !ok {
        st.selected = st.nodes.first().map(|n| n.id.clone());
    }
}

fn merge_nodes(st: &mut State, nodes: Vec<Node>, source: Option<&str>) -> (usize, usize) {
    let (mut added, mut updated) = (0, 0);
    for mut n in nodes {
        n.source = source.map(|s| s.to_string());
        if let Some(existing) = st.nodes.iter_mut().find(|e| e.id == n.id) {
            n.engine = existing.engine;
            *existing = n;
            updated += 1;
        } else {
            st.nodes.push(n);
            added += 1;
        }
    }
    fix_selection(st);
    (added, updated)
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(|c| c == '/' || c == '?' || c == '#').next().unwrap_or(rest).to_string()
}

/// A downloaded subscription: body plus what the provider says about it.
struct Fetched {
    body: String,
    userinfo: Option<String>,
    title: Option<String>,
}

async fn fetch(url: String, ua: String) -> Result<Fetched, String> {
    tokio::task::spawn_blocking(move || {
        let resp = ureq::get(&url)
            .set("User-Agent", &ua)
            .timeout(Duration::from_secs(20))
            .call()
            .map_err(|e| e.to_string())?;
        let userinfo = resp.header("subscription-userinfo").map(String::from);
        let title = crate::formats::subscription_title(resp.header("profile-title"), resp.header("content-disposition"));
        // into_string() stops at 10 MB, enough for any subscription.
        let body = resp.into_string().map_err(|e| e.to_string())?;
        Ok(Fetched { body, userinfo, title })
    })
    .await
    .map_err(|e| e.to_string())?
}

impl Controller {
    pub fn load() -> Self {
        let fd_limit = crate::util::raise_fd_limit();
        let dir = store::data_dir();
        let _ = std::fs::create_dir_all(dir.join("run"));
        let kill_armed = dir.join(KILL_FLAG).exists();
        let mut state = store::load_state(&dir);
        let logs = Arc::new(LogBuf::new(5000));
        logs.set_file(&dir.join("logs"));
        logs.push(format!(
            "[app] Mosaic {} started · {} · data {}",
            env!("CARGO_PKG_VERSION"),
            crate::diag::os_description(),
            dir.display()
        ));
        // A panic anywhere ends up in the log file too — the first thing to look at after a crash.
        {
            let logs = logs.clone();
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                logs.error(format!("[panic] {info}"));
                prev(info);
            }));
        }
        if state.proxy_dirty {
            match sys::unset_system_proxy() {
                Ok(()) => logs.push("[hub] system proxy reset after an unclean shutdown"),
                Err(e) => logs.push(format!("[hub] could not reset the system proxy: {e}")),
            }
            state.proxy_dirty = false;
            let _ = store::save_state(&dir, &state);
        }
        logs.push(format!("[hub] data: {}", dir.display()));
        if let Some((old, new)) = fd_limit {
            if new != old {
                logs.push(format!("[hub] open files limit: {old} → {new}"));
            } else if new < 8192 {
                logs.push(format!("[hub] open files limit is only {new}; with many connections Mosaic may hit \"Too many open files\""));
            }
        }
        #[cfg(target_os = "linux")]
        {
            for l in crate::linux::install_bundled(&engines::bundled_copy_dir()) {
                logs.push(l);
            }
            if sys::is_admin() {
                logs.push(
                    "[hub] Mosaic is running as root: the system proxy is then set for root, not for your desktop user. \
                     Start Mosaic as your normal user — TUN asks for the password itself.",
                );
            }
        }
        let close_to_tray = AtomicBool::new(state.settings.close_to_tray);
        let stats = Arc::new(Stats::default());
        stats.set_logs(logs.clone());
        stats.set_verbose(state.settings.verbose_log);
        if state.settings.verbose_log {
            logs.push("[app] verbose log is on: every connection is written to the log");
        }
        #[cfg(target_os = "linux")]
        for l in kill_switch_startup_report(kill_armed) {
            logs.push(l);
        }
        #[cfg(windows)]
        if kill_armed {
            logs.push("[killswitch] the last session did not end normally: the block (firewall filters) stays until you connect or press “Unblock”");
        }
        Self {
            inner: Mutex::new(Inner { state, session: None, latency: HashMap::new() }),
            stats,
            logs,
            close_to_tray,
            smart_kick: AtomicBool::new(true),
            check_kick: AtomicBool::new(false),
            app_update: std::sync::Mutex::new(None),
            tun_fd: std::sync::Mutex::new(None),
            progress: std::sync::Mutex::new(Progress::default()),
            health: std::sync::Mutex::new(Health { state: "unknown", ..Default::default() }),
            kill_armed: AtomicBool::new(kill_armed),
            dir,
            // "TUN is possible": Android asks for the VPN permission, Linux grants a capability
            // to sing-box (one password prompt) — neither needs Mosaic itself to run as root.
            admin: sys::is_admin() || cfg!(any(target_os = "android", target_os = "linux")),
        }
    }

    pub fn is_admin(&self) -> bool {
        self.admin
    }

    /// Android: hand over the VPN interface (from VpnService.Builder.establish().detachFd())
    /// used by the next TUN connect. Mosaic owns and closes it.
    /// Returns the previous descriptor if it was never used.
    pub fn set_tun_fd(&self, fd: Option<i32>) -> Option<i32> {
        std::mem::replace(&mut *self.tun_fd.lock().unwrap(), fd)
    }

    /// Android: Go engines cannot use the system resolver there, so node servers
    /// are resolved by Mosaic and handed to the engines as IP addresses
    /// (with the original name kept for TLS SNI and HTTP Host).
    async fn pin_servers(&self, nodes: Vec<Node>, dns: &str) -> Vec<Node> {
        let resolver = Resolver::udp_from_setting(dns);
        let mut out = Vec::with_capacity(nodes.len());
        for n in nodes {
            if n.server.parse::<std::net::IpAddr>().is_ok() {
                out.push(n);
                continue;
            }
            match resolver.resolve(&n.server).await {
                Ok(ips) if !ips.is_empty() => {
                    let ip = ips.iter().find(|i| i.is_ipv4()).unwrap_or(&ips[0]).to_string();
                    out.push(pin_node(n, ip));
                }
                Ok(_) | Err(_) => {
                    self.logs.push(format!("[hub] cannot resolve {} — the engine will try itself", n.server));
                    out.push(n);
                }
            }
        }
        out
    }

    pub async fn settings(&self) -> Settings {
        self.inner.lock().await.state.settings.clone()
    }

    /// Apply the configured start-up mode before an automatic connect.
    /// TUN without administrator rights falls back to the system proxy.
    pub async fn prepare_startup(&self) {
        let mut g = self.inner.lock().await;
        let wanted = match g.state.settings.startup_mode {
            StartupMode::Last => return,
            StartupMode::SystemProxy => Mode::SystemProxy,
            StartupMode::Proxy => Mode::Proxy,
            StartupMode::Tun if self.admin => Mode::Tun,
            StartupMode::Tun => {
                self.logs.push("[app] start-up mode is TUN, but Mosaic runs without administrator rights — using the system proxy");
                Mode::SystemProxy
            }
        };
        if g.state.settings.mode != wanted {
            g.state.settings.mode = wanted;
            self.save(&g.state);
        }
    }

    fn save(&self, st: &State) {
        if let Err(e) = store::save_state(&self.dir, st) {
            self.logs.push(format!("[hub] could not save settings: {e}"));
        }
    }

    fn write_config(&self, name: &str, cfg: &Value) -> Result<PathBuf, String> {
        let dir = self.dir.join("run");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{name}.json"));
        let bytes = serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(path)
    }

    /// Error message plus the engine's last log lines — usually the actual reason.
    fn engine_error(&self, msg: String, tag: &str) -> String {
        let tail = self.logs.tail_with(&format!("[{tag}]"), 6);
        if tail.is_empty() {
            msg
        } else {
            format!("{msg}\n{}", tail.join("\n"))
        }
    }

    /// Decode the geoip/geosite codes the rules use (cheap: only those codes).
    fn load_geo(&self, rules: &[Rule]) -> Arc<GeoData> {
        let (ip, site) = rules::geo_codes(rules);
        if ip.is_empty() && site.is_empty() {
            return Arc::new(GeoData::default());
        }
        let (data, warnings) = geo::load(&ip, &site);
        for w in warnings {
            self.logs.push(format!("[hub] {w}"));
        }
        for (code, set) in &data.site {
            if set.skipped_regex > 0 {
                self.logs.push(format!("[hub] geosite:{code}: {} regex entries skipped (not supported yet)", set.skipped_regex));
            }
        }
        Arc::new(data)
    }

    // ------------------------------------------------------------ queries

    pub async fn snapshot(&self) -> Snapshot {
        let g = self.inner.lock().await;
        let st = &g.state;
        let nodes = st
            .nodes
            .iter()
            .map(|n| {
                let eff = n.effective_engine();
                NodeView {
                    node: n.clone(),
                    summary: n.summary(),
                    supported: n.supported_engines(),
                    effective: eff.as_ref().ok().copied(),
                    effective_error: eff.err(),
                    latency: g.latency.get(&n.id).cloned(),
                }
            })
            .collect();
        let s = g.session.as_ref();
        let status = Status {
            connected: s.is_some(),
            mode: s.map(|s| s.mode),
            tun_stack: s.filter(|s| s.mode == Mode::Tun).map(|s| s.tun_stack),
            since_ms: s.map(|s| s.started_ms),
            up: self.stats.up.load(Relaxed),
            down: self.stats.down.load(Relaxed),
            active_conns: self.stats.active_count(),
            listen_port: s.map(|s| s.hub.port),
            lan: s.map_or(false, |s| s.lan),
            system_proxy: s.map_or(false, |s| s.system_proxy),
            tun: s.map_or(false, |s| s.tun_up()),
            dns_protected: s.map_or(false, |s| s.dns_protected()),
            kill_switch: match (self.kill_armed.load(Relaxed), s.is_some()) {
                (false, _) => "off",
                (true, true) => "armed",
                (true, false) => "blocking",
            },
            health: if s.is_some() { self.health.lock().unwrap().clone() } else { Health { state: "unknown", ..Default::default() } },
        };
        let engine_infos = EngineKind::ALL
            .iter()
            .map(|&k| EngineInfo {
                kind: k,
                name: k.label(),
                path: engines::find(k).map(|p| p.display().to_string()),
                running: s.map_or(false, |s| s.engines.iter().any(|p| p.kind == k)),
                pids: s
                    .map(|s| s.engines.iter().chain(s.front.iter()).filter(|p| p.kind == k).filter_map(|p| p.pid).collect())
                    .unwrap_or_default(),
                nodes: s.map_or(0, |s| s.upstreams.values().filter(|u| u.engine == k).count()),
            })
            .collect();
        let (geoip_file, geosite_file) = geo::file_status();
        Snapshot {
            version: env!("CARGO_PKG_VERSION"),
            nodes,
            subscriptions: st.subscriptions.clone(),
            rules: st.rules.clone(),
            settings: st.settings.clone(),
            selected: st.selected.clone(),
            status,
            engines: engine_infos,
            is_admin: self.admin,
            data_dir: self.dir.display().to_string(),
            // The native stack: wintun (Windows), a VpnService descriptor (Android), mosaic-tun-helper (Linux).
            native_tun_available: cfg!(feature = "native-tun") && cfg!(any(windows, target_os = "android", target_os = "linux")),
            app_update: self.app_update.lock().unwrap().clone(),
            lan_ip: if st.settings.allow_lan { lan_ip() } else { None },
            geoip_file,
            geosite_file,
            platform: std::env::consts::OS,
            engine_updates: engines::UPDATABLE,
            engines_updated: engines::UPDATABLE && engines::has_updates(),
        }
    }

    pub fn connections(&self) -> Vec<ConnView> {
        self.stats.connections()
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs.all()
    }

    /// Log records newer than `since` (for the Logs screen, polled incrementally).
    pub fn log_entries(&self, since: u64) -> Vec<crate::util::LogEntry> {
        self.logs.since(since)
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }

    /// A report for diagnosing a problem, with servers, accounts and public addresses
    /// replaced (see `diag`). Safe to paste into an issue.
    pub async fn diagnostics(&self) -> String {
        use std::fmt::Write;
        let versions = self.engine_versions().await;
        let snap = self.snapshot().await;
        let secrets = {
            let g = self.inner.lock().await;
            crate::diag::secrets(&g.state)
        };
        let st = &snap.status;
        let mut r = String::new();
        let _ = writeln!(r, "Mosaic diagnostic report");
        let _ = writeln!(r, "generated: {}", crate::util::fmt_utc(now_ms()));
        let _ = writeln!(r, "version:   {} ({})", snap.version, snap.platform);
        let _ = writeln!(r, "system:    {}", crate::diag::os_description());
        let _ = writeln!(r, "admin:     {}", snap.is_admin);
        let _ = writeln!(r, "\n## Status");
        let _ = writeln!(
            r,
            "connected: {} · mode {:?} · TUN stack {:?} · TUN up {} · system proxy {} · DNS protected {}",
            st.connected, st.mode, st.tun_stack, st.tun, st.system_proxy, st.dns_protected
        );
        let _ = writeln!(r, "health: {} {:?} {}", st.health.state, st.health.ms, st.health.error.clone().unwrap_or_default());
        let _ = writeln!(r, "kill switch: {} · active connections {} · up {} B · down {} B", st.kill_switch, st.active_conns, st.up, st.down);
        let _ = writeln!(r, "\n## Engines");
        for e in &snap.engines {
            let v = versions.iter().find(|v| v.kind == e.kind).and_then(|v| v.version.clone()).unwrap_or_else(|| "?".into());
            let _ = writeln!(r, "{}: {} · found {} · running {} · nodes {}", e.name, v, e.path.is_some(), e.running, e.nodes);
        }
        let _ = writeln!(r, "\n## Settings");
        let mut settings = serde_json::to_value(&snap.settings).unwrap_or(Value::Null);
        if let Some(o) = settings.as_object_mut() {
            let n = o.get("apps").and_then(|a| a.as_array()).map_or(0, |a| a.len());
            o.insert("apps".into(), Value::String(format!("{n} apps")));
        }
        let _ = writeln!(r, "{}", serde_json::to_string_pretty(&settings).unwrap_or_default());
        let _ = writeln!(r, "\n## Nodes ({}), subscriptions ({}), rules ({})", snap.nodes.len(), snap.subscriptions.len(), snap.rules.len());
        for n in &snap.nodes {
            let lat = n.latency.as_ref().map(|l| match l.ms {
                Some(ms) => format!("{ms} ms"),
                None => format!("error: {}", l.error.clone().unwrap_or_default()),
            });
            let sel = if snap.selected.as_deref() == Some(n.node.id.as_str()) { "*" } else { " " };
            let _ = writeln!(
                r,
                "{sel} {} · {} · engine {:?} → {:?} · {}",
                n.node.name,
                n.summary,
                n.node.engine,
                n.effective,
                lat.unwrap_or_else(|| "not tested".into())
            );
        }
        for s in &snap.subscriptions {
            let _ = writeln!(r, "subscription {} · {} · updated {} · {}", s.name, s.url, crate::util::fmt_utc(s.updated_ms), s.last_error.clone().unwrap_or_else(|| "ok".into()));
        }
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        for rule in &snap.rules {
            *kinds.entry(format!("{:?}→{}", rule.kind, rule.target)).or_default() += 1;
        }
        let _ = writeln!(r, "rules: {}", kinds.iter().map(|(k, n)| format!("{k} ×{n}")).collect::<Vec<_>>().join(", "));
        let _ = writeln!(r, "\n## Kill switch and routes");
        let _ = writeln!(r, "marker: {} · setting {} · mode {:?} · stack {:?}", self.dir.join(KILL_FLAG).exists(), snap.settings.kill_switch, snap.settings.mode, snap.settings.tun_stack);
        #[cfg(target_os = "linux")]
        {
            let _ = writeln!(r, "{}", crate::linux_tun::route_check().summary());
            for l in crate::linux_tun::ip_snapshot() {
                let _ = writeln!(r, "{l}");
            }
        }
        let errors = self.stats.recent_errors(40);
        let _ = writeln!(r, "\n## Recent failed connections ({})", errors.len());
        for c in &errors {
            let _ = writeln!(
                r,
                "{} {} {}→ {}:{} · rule {} → {} · {}",
                crate::util::fmt_utc(c.started_ms),
                c.network,
                c.process.as_deref().map(|p| format!("{p} ")).unwrap_or_default(),
                c.host,
                c.port,
                c.rule,
                c.outbound,
                c.error.clone().unwrap_or_default()
            );
        }
        // The log: the per-connection lines ("verbose log") are capped, so they do not push
        // out what happened to the hub, the engines and the TUN.
        let entries = self.logs.since(0);
        let conn: Vec<u64> = entries.iter().filter(|e| e.source == "conn").map(|e| e.seq).collect();
        let keep_conn: std::collections::HashSet<u64> = conn.iter().skip(conn.len().saturating_sub(400)).copied().collect();
        let picked: Vec<&crate::util::LogEntry> = entries.iter().filter(|e| e.source != "conn" || keep_conn.contains(&e.seq)).collect();
        let picked = &picked[picked.len().saturating_sub(2000)..];
        let _ = writeln!(r, "\n## Log (latest {} lines)", picked.len());
        for e in picked {
            let _ = writeln!(r, "{} {:<5} [{}] {}", crate::util::fmt_utc(e.ts), e.level.to_uppercase(), e.source, e.msg);
        }
        crate::diag::redact(&r, &secrets)
    }

    /// Write the report to the Downloads folder; returns its path.
    pub async fn save_diagnostics(&self) -> Result<(String, String), String> {
        let text = self.diagnostics().await;
        let dir = crate::diag::downloads_dir(&self.logs_dir());
        let stamp = crate::util::fmt_utc(now_ms()).replace([':', '-'], "").replace('T', "-");
        let path = dir.join(format!("mosaic-report-{}.txt", &stamp[..15]));
        std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.logs.push(format!("[app] diagnostic report saved: {}", path.display()));
        Ok((path.display().to_string(), text))
    }

    pub async fn engine_versions(&self) -> Vec<EngineVersion> {
        let mut out = Vec::new();
        for k in EngineKind::ALL {
            let version = match engines::find(k) {
                Some(exe) => process::version(&exe).await,
                None => None,
            };
            out.push(EngineVersion { kind: k, version });
        }
        out
    }

    // ------------------------------------------------------------ nodes & subscriptions

    pub async fn import_text(&self, text: &str) -> ImportSummary {
        let res = links::parse_text(text);
        let mut g = self.inner.lock().await;
        let (added, updated) = merge_nodes(&mut g.state, res.nodes, None);
        self.save(&g.state);
        if g.session.is_some() && added > 0 {
            self.logs.push("[hub] new nodes become available after reconnecting");
        }
        ImportSummary { added, updated, errors: res.errors }
    }

    pub async fn add_subscription(&self, url: &str) -> Result<ImportSummary, String> {
        let url = url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("A subscription URL must start with http:// or https://".into());
        }
        {
            let mut g = self.inner.lock().await;
            if !g.state.subscriptions.iter().any(|s| s.url == url) {
                g.state.subscriptions.push(Subscription {
                    url: url.clone(),
                    name: host_of(&url),
                    upload: None,
                    download: None,
                    total: None,
                    expire: None,
                    updated_ms: 0,
                    last_error: None,
                });
                self.save(&g.state);
            }
        }
        self.update_subscription(&url).await
    }

    pub async fn update_subscription(&self, url: &str) -> Result<ImportSummary, String> {
        let ua = self.inner.lock().await.state.settings.user_agent.clone();
        let fetched = fetch(url.to_string(), ua).await;
        let mut g = self.inner.lock().await;
        let st = &mut g.state;
        let fetched = match fetched {
            Ok(f) => f,
            Err(e) => {
                if let Some(s) = st.subscriptions.iter_mut().find(|s| s.url == url) {
                    s.last_error = Some(e.clone());
                }
                self.save(st);
                return Err(format!("Could not download the subscription: {e}"));
            }
        };
        let res = links::parse_text(&fetched.body);
        if res.nodes.is_empty() {
            let why = res.errors.first().cloned().unwrap_or_else(|| "empty response".into());
            if let Some(s) = st.subscriptions.iter_mut().find(|s| s.url == url) {
                s.last_error = Some(why.clone());
            }
            self.save(st);
            return Err(format!("No supported nodes in the subscription ({why})"));
        }
        let ids: Vec<String> = res.nodes.iter().map(|n| n.id.clone()).collect();
        st.nodes.retain(|n| n.source.as_deref() != Some(url) || ids.contains(&n.id));
        let (added, updated) = merge_nodes(st, res.nodes, Some(url));
        if let Some(s) = st.subscriptions.iter_mut().find(|s| s.url == url) {
            s.updated_ms = now_ms();
            s.last_error = None;
            if let Some(ui) = fetched.userinfo.as_deref().map(crate::formats::parse_userinfo) {
                s.upload = ui.upload;
                s.download = ui.download;
                s.total = ui.total;
                s.expire = ui.expire;
            }
            if let Some(t) = fetched.title.clone() {
                s.name = t;
            }
        }
        self.save(st);
        Ok(ImportSummary { added, updated, errors: res.errors })
    }

    pub async fn update_all_subscriptions(&self) -> ImportSummary {
        let urls: Vec<String> = self.inner.lock().await.state.subscriptions.iter().map(|s| s.url.clone()).collect();
        let mut total = ImportSummary::default();
        for url in urls {
            match self.update_subscription(&url).await {
                Ok(s) => {
                    total.added += s.added;
                    total.updated += s.updated;
                    total.errors.extend(s.errors);
                }
                Err(e) => total.errors.push(format!("{}: {e}", host_of(&url))),
            }
        }
        total
    }

    pub async fn remove_subscription(&self, url: &str) {
        let mut g = self.inner.lock().await;
        g.state.subscriptions.retain(|s| s.url != url);
        g.state.nodes.retain(|n| n.source.as_deref() != Some(url));
        fix_selection(&mut g.state);
        self.save(&g.state);
    }

    pub async fn remove_node(&self, id: &str) {
        let mut g = self.inner.lock().await;
        g.state.nodes.retain(|n| n.id != id);
        g.latency.remove(id);
        fix_selection(&mut g.state);
        self.save(&g.state);
    }

    /// Manual choice of the active node. Turns Smart Connect off.
    pub async fn select_node(&self, id: &str) -> Result<(), String> {
        self.select_inner(id, true).await
    }

    /// Instant switch: TCP and UDP both follow the routing table, so nothing restarts.
    async fn select_inner(&self, id: &str, manual: bool) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        let name = g.state.nodes.iter().find(|n| n.id == id).map(|n| n.name.clone()).ok_or("node not found")?;
        if manual && g.state.settings.smart_connect {
            g.state.settings.smart_connect = false;
            self.logs.push("[smart] Smart Connect turned off: a node was chosen manually");
        }
        g.state.selected = Some(id.to_string());
        self.save(&g.state);
        let inner = &mut *g;
        if let Some(s) = inner.session.as_ref() {
            if !s.upstreams.contains_key(id) {
                return Err("This node is not running — reconnect".into());
            }
            s.hub.update(routing_for(&inner.state, s));
            self.logs.push(format!("[hub] active node → {name}"));
            if manual {
                // A new node: unconfirmed until the next check, which comes right away.
                self.set_health("unknown", None, None);
                self.check_kick.store(true, Relaxed);
            }
        }
        Ok(())
    }

    /// Save an edited node. The id stays (rules that send traffic to it keep working); the
    /// share link it came from no longer describes it and is dropped. A running node is
    /// restarted only if something besides its name changed.
    pub async fn update_node(&self, mut node: Node) -> Result<(), String> {
        node.name = node.name.trim().to_string();
        node.server = node.server.trim().trim_matches(|c| c == '[' || c == ']').to_string();
        if node.server.is_empty() || node.server.contains(char::is_whitespace) {
            return Err("Enter the server address".into());
        }
        if node.port == 0 {
            return Err("Enter a port from 1 to 65535".into());
        }
        if node.name.is_empty() {
            node.name = format!("{}:{}", node.server, node.port);
        }
        node.effective_engine()?;
        let restart = {
            let mut g = self.inner.lock().await;
            let old = g.state.nodes.iter().find(|n| n.id == node.id).cloned().ok_or("node not found")?;
            node.source = old.source.clone();
            let same = |a: &Node, b: &Node| {
                a.server == b.server && a.port == b.port && a.protocol == b.protocol && a.transport == b.transport && a.security == b.security && a.engine == b.engine
            };
            let only_name = same(&old, &node);
            node.link = if only_name { old.link.clone() } else { String::new() };
            self.logs.push(format!(
                "[nodes] '{}' edited{}",
                node.name,
                if old.name != node.name { format!(" (was '{}')", old.name) } else { String::new() }
            ));
            if let Some(n) = g.state.nodes.iter_mut().find(|n| n.id == node.id) {
                *n = node.clone();
            }
            self.save(&g.state);
            let inner = &mut *g;
            match inner.session.as_mut() {
                Some(s) if s.upstreams.contains_key(&node.id) => {
                    if only_name {
                        if let Some(u) = s.upstreams.get_mut(&node.id) {
                            u.name = node.name.clone();
                        }
                        s.hub.update(routing_for(&inner.state, s));
                        false
                    } else {
                        true
                    }
                }
                _ => false,
            }
        };
        if restart {
            self.logs.push("[hub] an active node was edited — restarting engines");
            self.drop_session().await;
            self.connect().await?;
        }
        Ok(())
    }

    pub async fn set_node_engine(&self, id: &str, choice: EngineChoice) -> Result<(), String> {
        let running = {
            let mut g = self.inner.lock().await;
            let n = g.state.nodes.iter_mut().find(|n| n.id == id).ok_or("node not found")?;
            let mut probe = n.clone();
            probe.engine = choice;
            probe.effective_engine()?;
            n.engine = choice;
            self.save(&g.state);
            g.session.is_some()
        };
        if running {
            self.logs.push("[hub] node engine changed — restarting engines");
            self.drop_session().await;
            self.connect().await?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ rules & settings

    pub async fn set_rules(&self, rules: Vec<Rule>, final_target: String) {
        let mut g = self.inner.lock().await;
        g.state.rules = rules;
        g.state.settings.final_target = final_target;
        self.save(&g.state);
        let geo = if g.session.is_some() { Some(self.load_geo(&g.state.rules)) } else { None };
        let inner = &mut *g;
        if let (Some(s), Some(geo)) = (inner.session.as_mut(), geo) {
            s.geo = geo;
            s.hub.update(routing_for(&inner.state, s));
            self.logs.push("[hub] rules applied");
            // The sing-box adapter carries the per-app rules. Everything else lives in the hub and
            // was just swapped in without dropping connections; the adapter (and with it the
            // whole tunnel) is restarted only when the per-app rules themselves changed.
            let apps = app_rules(&inner.state, s);
            if s.front.is_some() && apps != s.front_apps {
                if let (Some(old), Some(dp)) = (s.front.take(), s.direct_port) {
                    self.logs.push("[hub] rules by application changed — restarting the TUN adapter");
                    old.stop().await;
                    match self.start_front(s.hub.port, dp, &s.server_domains, &s.server_ips, &inner.state.settings, &apps).await {
                        Ok(p) => {
                            s.front = Some(p);
                            s.front_apps = apps;
                        }
                        Err(e) => self.logs.push(format!("[hub] TUN adapter restart failed: {e}")),
                    }
                }
            }
        }
    }

    /// Turn Smart Connect on or off (on: re-test right away).
    pub async fn set_smart(&self, on: bool) {
        let mut g = self.inner.lock().await;
        g.state.settings.smart_connect = on;
        self.save(&g.state);
        self.smart_kick.store(on, Relaxed);
        self.logs.push(if on { "[smart] Smart Connect on" } else { "[smart] Smart Connect off" });
    }

    // ------------------------------------------------------------ updates

    pub async fn check_app_update(&self) -> Option<AppUpdate> {
        match updates::check_app().await {
            Ok(u) => {
                if let Some(u) = &u {
                    self.logs.push(format!("[update] Mosaic {} is available", u.version));
                }
                *self.app_update.lock().unwrap() = u.clone();
                u
            }
            Err(e) => {
                self.logs.push(format!("[update] cannot check for updates: {e}"));
                None
            }
        }
    }

    /// Download the new installer (checksum verified); the UI then runs it.
    pub async fn download_app_update(&self) -> Result<PathBuf, String> {
        let dir = std::env::temp_dir().join("mosaic-update");
        let p = updates::download_app_installer(&dir).await?;
        self.logs.push(format!("[update] installer downloaded: {}", p.display()));
        Ok(p)
    }

    pub async fn check_engine_updates(&self) -> Vec<EngineUpdate> {
        let mut out = Vec::new();
        if !engines::UPDATABLE {
            return out;
        }
        for kind in EngineKind::ALL {
            let current = match engines::find(kind) {
                Some(exe) => process::version(&exe).await,
                None => None,
            };
            let (latest, error) = match updates::engine_latest(kind).await {
                Ok(r) => (Some(r.tag), None),
                Err(e) => (None, Some(e)),
            };
            let available = match (&latest, &current) {
                (Some(l), Some(c)) => updates::is_newer(l, c),
                (Some(_), None) => true,
                _ => false,
            };
            out.push(EngineUpdate { kind, current, latest, available, error });
        }
        out
    }

    /// Start the nodes served by `kind` once to prove the engine works with our configs.
    async fn validate_engine(&self, kind: EngineKind) -> Result<(), String> {
        let nodes: Vec<Node> = {
            let g = self.inner.lock().await;
            g.state.nodes.iter().filter(|n| n.effective_engine().ok() == Some(kind)).cloned().collect()
        };
        if nodes.is_empty() {
            return Ok(());
        }
        let (procs, ups) = self.start_engines(&nodes).await?;
        for p in procs {
            p.stop().await;
        }
        if ups.is_empty() {
            Err(format!("{} did not start with your nodes", kind.label()))
        } else {
            Ok(())
        }
    }

    /// Install the latest engine release. Verified download → staged → swapped
    /// in → checked with your nodes (and reconnected if you were connected).
    /// Any failure restores the previous version.
    pub async fn update_engine(&self, kind: EngineKind) -> Result<String, String> {
        if !engines::UPDATABLE {
            return Err("engines are updated together with the app on this platform".into());
        }
        let staging = self.dir.join("engines.staging").join(kind.slug());
        let backup = self.dir.join("engines.bak").join(kind.slug());
        let target = engines::updated_dir();
        self.logs.push(format!("[update] downloading the latest {}…", kind.label()));
        let (tag, files) = updates::stage_engine(kind, &staging).await?;
        if process::version(&staging.join(kind.exe_name())).await.is_none() {
            return Err(format!("{} {tag} does not start on this system", kind.label()));
        }
        let was_connected = self.is_connected().await;
        if was_connected {
            self.drop_session().await;
        }
        if let Err(e) = updates::swap_in(&files, &staging, &target, &backup) {
            updates::roll_back(&files, &target, &backup);
            if was_connected {
                let _ = self.connect().await;
            }
            return Err(format!("could not replace files: {e}"));
        }
        let check = if was_connected { self.connect().await } else { self.validate_engine(kind).await };
        if let Err(e) = check {
            self.logs.push(format!("[update] {} {tag} failed ({e}) — restoring the previous version", kind.label()));
            if was_connected {
                self.drop_session().await;
            }
            updates::roll_back(&files, &target, &backup);
            if was_connected {
                let _ = self.connect().await;
            }
            return Err(format!("{} {tag} did not work with your nodes; the previous version was restored. {e}", kind.label()));
        }
        let _ = std::fs::remove_dir_all(&staging);
        self.logs.push(format!("[update] {} updated to {tag}", kind.label()));
        Ok(tag)
    }

    /// Forget all updated engines and use the ones shipped with Mosaic.
    pub async fn reset_engines(&self) -> Result<(), String> {
        if !engines::UPDATABLE {
            return Ok(());
        }
        let was_connected = self.is_connected().await;
        if was_connected {
            self.drop_session().await;
        }
        let res = match std::fs::remove_dir_all(engines::updated_dir()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
        self.logs.push("[update] using the engines shipped with Mosaic");
        if was_connected {
            self.connect().await?;
        }
        res
    }

    // ------------------------------------------------------------ background work

    /// Smart Connect and scheduled subscription refresh. Run once per process,
    /// e.g. `tokio::spawn(ctl.clone().run_background())`.
    pub async fn run_background(self: Arc<Self>) {
        let mut tick: u64 = 0;
        let mut fails = 0u32;
        let mut last_full: Option<Instant> = None;
        let mut sub_attempts: HashMap<String, Instant> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            tick += 1;
            let settings = self.settings().await;

            // Subscriptions: check every 10 minutes (and 10 s after start).
            if settings.sub_update_hours > 0 && (tick == 1 || tick % 60 == 0) {
                let max_age = settings.sub_update_hours as u64 * 3600 * 1000;
                let subs: Vec<Subscription> = self.inner.lock().await.state.subscriptions.clone();
                for sub in subs {
                    let stale = now_ms().saturating_sub(sub.updated_ms) > max_age;
                    let recently_tried = sub_attempts.get(&sub.url).map_or(false, |t| t.elapsed() < Duration::from_secs(3600));
                    if stale && !recently_tried {
                        sub_attempts.insert(sub.url.clone(), Instant::now());
                        match self.update_subscription(&sub.url).await {
                            Ok(r) => self.logs.push(format!("[subs] {} refreshed: {} new, {} updated", host_of(&sub.url), r.added, r.updated)),
                            Err(e) => self.logs.push(format!("[subs] {}: {e}", host_of(&sub.url))),
                        }
                    }
                }
            }

            // Mosaic updates: 30 s after start, then daily.
            // Android: new versions come as an APK, not through the in-app installer.
            if settings.check_updates && !cfg!(target_os = "android") && (tick == 3 || tick % 8640 == 0) {
                self.check_app_update().await;
            }

            if settings.smart_connect {
                let kicked = self.smart_kick.swap(false, Relaxed);
                if kicked {
                    last_full = None;
                    fails = 0;
                }
                self.smart_tick(&settings, tick, &mut fails, &mut last_full).await;
            } else {
                last_full = None;
                if tick % 2 == 0 || self.check_kick.swap(false, Relaxed) {
                    self.health_tick(&settings, &mut fails).await;
                }
            }
        }
    }

    /// Without Smart Connect: check the active node every 20 s and show when it stops
    /// carrying traffic (two failures in a row), instead of a misleading "connected".
    async fn health_tick(&self, settings: &Settings, fails: &mut u32) {
        let port = {
            let g = self.inner.lock().await;
            let Some(s) = g.session.as_ref() else {
                *fails = 0;
                return;
            };
            g.state.selected.as_ref().and_then(|id| s.upstreams.get(id)).map(|u| u.port)
        };
        if !probe_url_ok(&settings.test_url) {
            return;
        }
        let Some(port) = port else {
            self.set_health("degraded", None, Some("the selected node is not running".into()));
            return;
        };
        match Self::probe_node(port, &settings.test_url).await {
            Ok((ms, _)) => {
                if *fails >= 2 {
                    self.logs.push("[check] the node answers again");
                }
                *fails = 0;
                self.set_health("ok", Some(ms), None);
            }
            Err(e) => {
                *fails += 1;
                if *fails == 2 {
                    self.logs.push(format!("[check] the node stopped answering: {e}"));
                }
                if *fails >= 2 {
                    self.set_health("degraded", None, Some(e));
                }
            }
        }
    }

    async fn smart_tick(&self, settings: &Settings, tick: u64, fails: &mut u32, last_full: &mut Option<Instant>) {
        let (upstreams, selected) = {
            let g = self.inner.lock().await;
            match g.session.as_ref() {
                Some(s) => (s.upstreams.clone(), g.state.selected.clone()),
                None => return,
            }
        };
        if upstreams.is_empty() {
            return;
        }
        // Health check of the active node every 20 s.
        let mut current_ms: Option<u32> = None;
        if tick % 2 == 0 || last_full.is_none() {
            if let Some(u) = selected.as_ref().and_then(|id| upstreams.get(id)) {
                match hub::url_test(u.port, &settings.test_url).await {
                    Ok(ms) => {
                        *fails = 0;
                        current_ms = Some(ms);
                        self.set_health("ok", Some(ms), None);
                    }
                    Err(e) => {
                        *fails += 1;
                        if *fails >= 2 {
                            self.set_health("degraded", None, Some(e));
                        }
                    }
                }
            } else {
                *fails = 2;
            }
        }
        let full_due = last_full.map_or(true, |t| t.elapsed() >= Duration::from_secs(180));
        if !(full_due || *fails >= 2) {
            return;
        }
        *last_full = Some(Instant::now());
        let mut set = JoinSet::new();
        for (id, u) in &upstreams {
            let (id, port, url) = (id.clone(), u.port, settings.test_url.clone());
            set.spawn(async move { (id, hub::url_probe(port, &url).await) });
        }
        let mut results: Vec<(String, Result<(u32, Option<u32>), String>)> = Vec::new();
        while let Some(r) = set.join_next().await {
            if let Ok(x) = r {
                results.push(x);
            }
        }
        let now = now_ms();
        {
            let mut g = self.inner.lock().await;
            for (id, r) in &results {
                let l = match r {
                    Ok((ms, rtt)) => Latency { ms: Some(*ms), rtt: *rtt, error: None, at_ms: now },
                    Err(e) => Latency { ms: None, rtt: None, error: Some(e.clone()), at_ms: now },
                };
                g.latency.insert(id.clone(), l);
            }
        }
        let best = results.iter().filter_map(|(id, r)| r.as_ref().ok().map(|(ms, _)| (id.clone(), *ms))).min_by_key(|(_, ms)| *ms);
        let Some((best_id, best_ms)) = best else {
            self.logs.push("[smart] no node answered the test");
            if *fails >= 2 {
                self.set_health("degraded", None, Some("no node answers".into()));
            }
            return;
        };
        let current = selected.as_ref().and_then(|id| results.iter().find(|(i, _)| i == id)).and_then(|(_, r)| r.as_ref().ok().map(|(ms, _)| *ms));
        let current = current.or(current_ms);
        let switch = match current {
            None => true,                                                   // active node is down
            Some(cur) => *fails >= 2 || cur > best_ms + best_ms * 3 / 10 + 50, // clearly faster
        };
        if switch && selected.as_deref() != Some(best_id.as_str()) {
            let name = upstreams.get(&best_id).map(|u| u.name.clone()).unwrap_or_default();
            match self.select_inner(&best_id, false).await {
                Ok(()) => {
                    self.logs.push(format!("[smart] switched to {name} ({best_ms} ms)"));
                    self.set_health("ok", Some(best_ms), None);
                }
                Err(e) => self.logs.push(format!("[smart] {e}")),
            }
            *fails = 0;
        }
    }

    pub async fn set_settings(&self, mut new: Settings) -> Result<(), String> {
        if new.listen_port < 1024 {
            new.listen_port = 7890;
        }
        let mut autostart_err = None;
        let restart = {
            let mut g = self.inner.lock().await;
            let old = g.state.settings.clone();
            if old.autostart != new.autostart {
                let exe = std::env::current_exe().map_err(|e| e.to_string())?;
                match sys::set_autostart(new.autostart, &exe, self.admin) {
                    Ok(()) => self.logs.push(format!(
                        "[hub] autostart {}",
                        if !new.autostart { "off" } else if self.admin { "on (elevated task, TUN works at logon)" } else { "on" }
                    )),
                    Err(e) => {
                        new.autostart = old.autostart;
                        autostart_err = Some(format!("Autostart: {e}"));
                    }
                }
            }
            self.close_to_tray.store(new.close_to_tray, Relaxed);
            self.stats.set_verbose(new.verbose_log);
            // What changed, for the log (a frequent first question when something broke).
            if let (Ok(Value::Object(a)), Ok(Value::Object(b))) = (serde_json::to_value(&old), serde_json::to_value(&new)) {
                let changed: Vec<String> = b
                    .iter()
                    .filter(|(k, v)| a.get(*k) != Some(*v))
                    .map(|(k, v)| if k == "apps" { format!("{k} → {} apps", v.as_array().map_or(0, |x| x.len())) } else { format!("{k}: {} → {v}", a.get(k).cloned().unwrap_or(Value::Null)) })
                    .collect();
                if !changed.is_empty() {
                    self.logs.push(format!("[settings] {}", changed.join(", ")));
                }
            }
            g.state.settings = new.clone();
            self.save(&g.state);
            let inner = &mut *g;
            if let Some(s) = &inner.session {
                s.hub.update(routing_for(&inner.state, s));
            }
            inner.session.is_some()
                && (old.mode != new.mode
                    || old.listen_port != new.listen_port
                    || old.block_quic != new.block_quic
                    || old.tun_stack != new.tun_stack
                    || old.direct_dns != new.direct_dns
                    || old.dns_leak_protection != new.dns_leak_protection
                    || old.allow_lan != new.allow_lan
                    || old.kill_switch != new.kill_switch
                    || (new.mode == Mode::Tun && old.remote_dns != new.remote_dns && new.tun_stack == TunStack::Singbox))
        };
        if restart {
            self.drop_session().await;
            self.connect().await?;
        }
        match autostart_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------ session

    async fn spawn_engine(&self, kind: EngineKind, slots: &[Slot]) -> Result<EngineProcess, String> {
        let exe = engines::find(kind).ok_or_else(|| {
            format!("{} not found: put {} into the engines folder ({})", kind.label(), kind.exe_name(), if cfg!(windows) { "scripts\\get-engines.ps1" } else { "scripts/get-engines-linux.sh" })
        })?;
        let cfg = engines::compile(kind, slots)?;
        let path = self.write_config(kind.slug(), &cfg)?;
        let mut p = EngineProcess::spawn(kind, "engine", &exe, &path, self.logs.clone()).await?;
        let ports: Vec<u16> = slots.iter().map(|(_, port)| *port).collect();
        if let Err(e) = p.wait_ports(&ports, Duration::from_secs(8)).await {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let msg = self.engine_error(e, kind.label());
            p.stop().await;
            return Err(msg);
        }
        Ok(p)
    }

    /// Start one process per engine with one SOCKS inbound per node.
    async fn start_engines(&self, nodes: &[Node]) -> Result<(Vec<EngineProcess>, HashMap<String, Upstream>), String> {
        let mut plan: Vec<(Node, EngineKind)> = Vec::new();
        for n in nodes {
            match n.effective_engine() {
                Ok(k) => plan.push((n.clone(), k)),
                Err(e) => self.logs.push(format!("[hub] '{}' skipped: {e}", n.name)),
            }
        }
        if plan.is_empty() {
            return Err("No node is supported by the available engines".into());
        }
        let ports = process::alloc_ports(plan.len())?;
        let mut groups: HashMap<EngineKind, Vec<Slot>> = HashMap::new();
        let mut upstreams = HashMap::new();
        for ((n, k), port) in plan.into_iter().zip(ports) {
            upstreams.insert(n.id.clone(), Upstream { port, name: n.name.clone(), engine: k, udp: n.protocol.udp() });
            groups.entry(k).or_default().push((n, port));
        }
        let mut procs = Vec::new();
        let mut errors = Vec::new();
        for (kind, slots) in groups {
            match self.spawn_engine(kind, &slots).await {
                Ok(p) => procs.push(p),
                Err(e) => {
                    // Keep going with the other engine; its nodes stay usable.
                    self.logs.push(format!("[hub] {e}"));
                    for (n, _) in &slots {
                        upstreams.remove(&n.id);
                    }
                    errors.push(e);
                }
            }
        }
        if procs.is_empty() {
            return Err(errors.join("\n"));
        }
        Ok((procs, upstreams))
    }

    async fn start_front(
        &self,
        hub_port: u16,
        direct_port: u16,
        domains: &[String],
        server_ips: &[std::net::IpAddr],
        settings: &Settings,
        apps: &[(Vec<String>, singbox::FrontTarget)],
    ) -> Result<EngineProcess, String> {
        let engine = engines::find(EngineKind::Singbox).ok_or("TUN mode needs sing-box in the engines folder")?;
        // Linux: the adapter runs from its own copy that carries the TUN capability.
        #[cfg(target_os = "linux")]
        let exe = crate::linux::tun_binary(&engine, &engines::bundled_copy_dir())?;
        #[cfg(not(target_os = "linux"))]
        let exe = engine;
        let mut procs: Vec<String> = vec![EngineKind::Xray.exe_name().into(), EngineKind::Singbox.exe_name().into()];
        if let Some(name) = std::env::current_exe().ok().and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())) {
            procs.push(name);
        }
        let dns = if settings.direct_dns.trim().is_empty() { "8.8.8.8" } else { settings.direct_dns.trim() };
        let dns = dns.split(':').next().unwrap_or("8.8.8.8");
        let remote = resolver::parse_dns_server(&settings.remote_dns, "1.1.1.1:53");
        // With IPv6 turned off in the system, sing-box cannot give the interface its IPv6
        // address ("set ipv6 address: Element not found") and exits: retry IPv4-only.
        for ipv6 in [true, false] {
            let cfg = singbox::compile_tun(hub_port, direct_port, &procs, domains, server_ips, dns, &remote.ip().to_string(), settings.block_quic, apps, ipv6);
            let path = self.write_config("tun", &cfg)?;
            match self.try_front(&exe, &path, direct_port).await {
                Ok(p) => {
                    let v = if ipv6 { "" } else { ", IPv4 only" };
                    self.logs.push(format!("[hub] TUN interface 'Mosaic' is up (sing-box adapter{v})"));
                    return Ok(p);
                }
                Err(e) if ipv6 && e.to_ascii_lowercase().contains("ipv6") => {
                    self.logs.push("[hub] IPv6 is not available on this system — starting the TUN adapter with IPv4 only");
                }
                Err(e) => return Err(e),
            }
        }
        Err("TUN adapter failed to start".into())
    }

    async fn try_front(&self, exe: &std::path::Path, path: &std::path::Path, direct_port: u16) -> Result<EngineProcess, String> {
        let mut p = EngineProcess::spawn(EngineKind::Singbox, "tun", exe, path, self.logs.clone()).await?;
        if let Err(e) = p.wait_ports(&[direct_port], Duration::from_secs(8)).await {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let msg = self.engine_error(format!("TUN adapter failed to start: {e}"), "sing-box/tun");
            p.stop().await;
            return Err(msg);
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(st) = p.exited() {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Err(self.engine_error(format!("TUN adapter exited ({st})"), "sing-box/tun"));
            }
        }
        Ok(p)
    }

    /// Current connect step, without taking the controller lock.
    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    fn step(&self, step: &'static str, detail: Option<String>) {
        let mut p = self.progress.lock().unwrap();
        if p.step.is_none() {
            p.since_ms = now_ms();
        }
        p.step = Some(step);
        p.detail = detail;
    }

    fn set_health(&self, state: &'static str, ms: Option<u32>, error: Option<String>) {
        *self.health.lock().unwrap() = Health { state, error, checked_ms: now_ms(), ms };
    }

    /// One node through its engine, with a hard limit and one retry.
    async fn probe_node(port: u16, url: &str) -> Result<(u32, Option<u32>), String> {
        let mut last = String::new();
        for _ in 0..2 {
            match tokio::time::timeout(Duration::from_secs(6), hub::url_probe(port, url)).await {
                Ok(Ok(r)) => return Ok(r),
                Ok(Err(e)) => last = e,
                Err(_) => last = "no answer within 6 s".into(),
            }
        }
        Err(last)
    }

    /// Before the system is switched over (system proxy / TUN): does the selected node
    /// actually carry traffic? If not, and Smart Connect is on, take the fastest node that
    /// answers. Otherwise refuse: a "connected" VPN through a dead server only misleads.
    /// Errors start with a tag the UI recognises: `[node-down] name :: detail` or `[no-node]`.
    async fn check_before_switch(&self, st: &mut State, latency: &mut HashMap<String, Latency>, s: &Session) -> Result<(), String> {
        let url = st.settings.test_url.clone();
        if !probe_url_ok(&url) {
            self.logs.push("[check] skipped: the latency test URL is not http:// or https://");
            return Ok(());
        }
        let selected = st.selected.clone().unwrap_or_default();
        let name = s.upstreams.get(&selected).map(|u| u.name.clone()).unwrap_or_else(|| selected.clone());
        self.step("check", Some(name.clone()));
        let first = match s.upstreams.get(&selected) {
            Some(u) => Self::probe_node(u.port, &url).await,
            None => Err("its engine did not start".into()),
        };
        let now = now_ms();
        let record = |latency: &mut HashMap<String, Latency>, id: &str, r: &Result<(u32, Option<u32>), String>| {
            let l = match r {
                Ok((ms, rtt)) => Latency { ms: Some(*ms), rtt: *rtt, error: None, at_ms: now },
                Err(e) => Latency { ms: None, rtt: None, error: Some(e.clone()), at_ms: now },
            };
            latency.insert(id.to_string(), l);
        };
        record(latency, &selected, &first);
        let err = match first {
            Ok((ms, _)) => {
                self.logs.push(format!("[check] {name} answers ({ms} ms)"));
                self.set_health("ok", Some(ms), None);
                return Ok(());
            }
            Err(e) => e,
        };
        self.logs.push(format!("[check] {name} is not responding: {err}"));
        if !st.settings.smart_connect {
            return Err(format!("[node-down] {name} :: {err}"));
        }
        self.step("check", Some("*".into()));
        let mut set = JoinSet::new();
        for (id, u) in &s.upstreams {
            if *id == selected {
                continue;
            }
            let (id, port, url) = (id.clone(), u.port, url.clone());
            set.spawn(async move { (id, Self::probe_node(port, &url).await) });
        }
        let mut best: Option<(String, u32)> = None;
        while let Some(r) = set.join_next().await {
            let Ok((id, r)) = r else { continue };
            record(latency, &id, &r);
            if let Ok((ms, _)) = r {
                if best.as_ref().map_or(true, |(_, b)| ms < *b) {
                    best = Some((id, ms));
                }
            }
        }
        let Some((id, ms)) = best else {
            return Err(format!("[no-node] {name} :: {err}"));
        };
        let new_name = s.upstreams.get(&id).map(|u| u.name.clone()).unwrap_or_default();
        st.selected = Some(id);
        self.save(st);
        s.hub.update(routing_for(st, s));
        self.logs.push(format!("[smart] {name} is not responding — switched to {new_name} ({ms} ms)"));
        self.set_health("ok", Some(ms), None);
        Ok(())
    }

    pub async fn connect(&self) -> Result<(), String> {
        let started = Instant::now();
        let r = self.connect_inner().await;
        match &r {
            Ok(()) => self.logs.push(format!("[hub] ready in {} ms", started.elapsed().as_millis())),
            Err(e) => self.logs.error(format!("[hub] connect failed after {} ms: {e}", started.elapsed().as_millis())),
        }
        r
    }

    async fn connect_inner(&self) -> Result<(), String> {
        let _done = ProgressDone(self);
        self.step("prepare", None);
        let mut g = self.inner.lock().await;
        if g.session.is_some() {
            return Ok(());
        }
        if g.state.nodes.is_empty() {
            return Err("Add a node or a subscription first".into());
        }
        fix_selection(&mut g.state);
        let mut settings = g.state.settings.clone();
        if cfg!(target_os = "android") {
            // Android: the TUN is always Mosaic's own stack on the VpnService interface,
            // and there is no system proxy to set.
            settings.tun_stack = TunStack::Native;
            if settings.mode == Mode::SystemProxy {
                settings.mode = Mode::Tun;
            }
        }
        let tun = settings.mode == Mode::Tun;
        // Kill switch on, but with a TUN that cannot hold it: refuse (fail-closed) — otherwise
        // the switch looks on while nothing would be blocked if Mosaic stopped.
        if tun && settings.kill_switch && !cfg!(target_os = "android") && !kill_switch_supported(&settings) {
            let e = "the kill switch works on Linux only with Mosaic's own TUN stack, not with the sing-box adapter";
            self.logs.error(format!("[killswitch] cannot connect: {e}"));
            return Err(format!(
                "Kill switch: {e}.\n\nNot connected, so that nothing leaks unnoticed. In Settings choose “TUN implementation: Mosaic TUN”, \
                 or turn the kill switch off."
            ));
        }
        #[cfg(target_os = "linux")]
        if tun {
            // Linux, both stacks: the program that creates the interface gets a file capability
            // (one password prompt) — the sing-box adapter copy, or Mosaic's own TUN helper.
            let exe = if settings.tun_stack == TunStack::Native {
                crate::linux_tun::helper_binary(&engines::bundled_copy_dir())?
            } else {
                let engine = engines::find(EngineKind::Singbox).ok_or("TUN mode needs sing-box in the engines folder")?;
                crate::linux::tun_binary(&engine, &engines::bundled_copy_dir())?
            };
            if !sys::is_admin() && !crate::linux::tun_ready(&exe) {
                self.logs.push("[hub] asking for permission to create the TUN interface (one-time)…");
                tokio::task::spawn_blocking(move || crate::linux::grant_tun(&exe))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| format!("TUN: {e}"))?;
            }
        }
        if tun && !self.admin {
            return Err("TUN mode requires running Mosaic as administrator".into());
        }
        if tun && settings.tun_stack == TunStack::Native && !cfg!(feature = "native-tun") {
            return Err("This build has no native TUN stack. Switch the TUN stack to sing-box in Settings, or use the build with the native-tun feature.".into());
        }
        self.logs.push("[hub] connecting…");
        {
            let st = &g.state;
            let node = st.selected.as_ref().and_then(|id| st.nodes.iter().find(|n| &n.id == id)).map(|n| n.name.clone()).unwrap_or_else(|| "none".into());
            let ks = if !settings.kill_switch {
                "off"
            } else if cfg!(target_os = "android") {
                "Android system setting"
            } else if tun {
                "on"
            } else {
                "on in settings, not used outside TUN"
            };
            self.logs.push(format!(
                "[hub] session: mode {:?}{} · node {node} · kill switch {ks} · WebRTC protection {} · DNS leak protection {} · \
                 block QUIC {} · LAN {} · final → {} · direct DNS {} · remote DNS {} · rules {} on · smart connect {} · verbose log {}",
                settings.mode,
                if tun { format!(" ({:?} stack)", settings.tun_stack) } else { String::new() },
                on_off(settings.webrtc_protection),
                on_off(settings.dns_leak_protection),
                on_off(settings.block_quic),
                on_off(settings.allow_lan),
                settings.final_target,
                settings.direct_dns,
                settings.remote_dns,
                st.rules.iter().filter(|r| r.enabled).count(),
                on_off(settings.smart_connect),
                on_off(settings.verbose_log),
            ));
        }
        // Linux: a blocking interface from an earlier session (the kill switch, or Mosaic was
        // killed) is taken over only by Mosaic's own TUN; anything else must remove it first,
        // or it keeps swallowing traffic that should go direct.
        #[cfg(target_os = "linux")]
        if !(tun && settings.tun_stack == TunStack::Native) && std::path::Path::new("/sys/class/net/Mosaic").exists() {
            self.logs.push("[killswitch] removing the blocking 'Mosaic' interface left from an earlier session (this connection does not use Mosaic's TUN stack)");
            self.release_kill_switch_os();
        }
        // Linux, kill switch in force: the old interface still takes all traffic, so Mosaic's
        // own lookups and connections must go around it from the start.
        #[cfg(target_os = "linux")]
        if tun && self.kill_armed.load(Relaxed) {
            if let Some((phys, _)) = crate::linux_tun::physical_default_route() {
                crate::net::set_bypass_device(&phys);
                crate::net::set_bypass_interface(crate::linux_tun::ifindex(&phys).unwrap_or(1));
            }
        }
        let nodes = g.state.nodes.clone();
        // Android always, Linux in TUN: engines get IP addresses (resolved here, before the TUN
        // is up) — Go cannot use Android's DNS, and on Linux the TUN would answer with fake IPs.
        let pin = cfg!(target_os = "android") || (cfg!(target_os = "linux") && tun);
        let engine_nodes = if pin { self.pin_servers(nodes.clone(), &settings.direct_dns).await } else { nodes.clone() };
        let mut server_ips: Vec<std::net::IpAddr> = engine_nodes.iter().filter_map(|n| n.server.parse().ok()).collect();
        server_ips.push(resolver::parse_dns_server(&settings.direct_dns, "8.8.8.8:53").ip());
        server_ips.sort();
        server_ips.dedup();
        self.step("engines", None);
        let (engine_procs, upstreams) = self.start_engines(&engine_nodes).await?;
        let direct_port = if tun && settings.tun_stack == TunStack::Singbox {
            match process::alloc_ports(1) {
                Ok(p) => Some(p[0]),
                Err(e) => {
                    for p in engine_procs {
                        p.stop().await;
                    }
                    return Err(e);
                }
            }
        } else {
            None
        };
        // In TUN modes the OS resolver would answer with fake IPs: use plain DNS.
        let resolver = Arc::new(if tun { Resolver::udp_from_setting(&settings.direct_dns) } else { Resolver::system() });
        let geo = self.load_geo(&g.state.rules);
        let draft = Session {
            hub: match Hub::start(
                settings.listen_port,
                settings.allow_lan,
                Routing::basic(Vec::new(), "block"),
                self.stats.clone(),
                self.logs.clone(),
            )
            .await
            {
                Ok(h) => h,
                Err(e) => {
                    for p in engine_procs {
                        p.stop().await;
                    }
                    return Err(e);
                }
            },
            engines: engine_procs,
            front: None,
            front_apps: Vec::new(),
            #[cfg(feature = "native-tun")]
            native: None,
            upstreams,
            mode: settings.mode,
            tun_stack: settings.tun_stack,
            lan: settings.allow_lan,
            system_proxy: false,
            started_ms: now_ms(),
            direct_port,
            server_domains: server_domains(&nodes),
            server_ips,
            geo,
            resolver,
            remote_cache: Arc::new(DnsCache::default()),
            logs: self.logs.clone(),
            #[cfg(windows)]
            dns_guard: None,
        };
        let mut s = draft;
        s.hub.update(routing_for(&g.state, &s));
        // The node is checked through its engine while the system is still untouched:
        // a dead node costs the user nothing, not even a moment without internet.
        {
            let inner = &mut *g;
            if let Err(e) = self.check_before_switch(&mut inner.state, &mut inner.latency, &s).await {
                self.teardown(s).await;
                return Err(e);
            }
        }
        self.step("route", None);
        let res: Result<(), String> = match (settings.mode, settings.tun_stack) {
            (Mode::SystemProxy, _) => match sys::set_system_proxy(settings.listen_port) {
                Ok(true) => {
                    s.system_proxy = true;
                    g.state.proxy_dirty = true;
                    self.save(&g.state);
                    self.logs.push(format!("[hub] system proxy → 127.0.0.1:{}", settings.listen_port));
                    Ok(())
                }
                Ok(false) => {
                    self.logs.push(format!(
                        "[hub] the system proxy is not set automatically on this OS; use 127.0.0.1:{}",
                        settings.listen_port
                    ));
                    Ok(())
                }
                Err(e) => Err(format!("Could not enable the system proxy: {e}")),
            },
            (Mode::Proxy, _) => {
                let host = if settings.allow_lan { "0.0.0.0" } else { "127.0.0.1" };
                self.logs.push(format!(
                    "[hub] proxy mode: set SOCKS5 or HTTP proxy {host}:{} in your apps",
                    settings.listen_port
                ));
                Ok(())
            }
            (Mode::Tun, TunStack::Singbox) => {
                let dp = direct_port.unwrap_or(0);
                let apps = app_rules(&g.state, &s);
                match self.start_front(s.hub.port, dp, &s.server_domains, &s.server_ips, &settings, &apps).await {
                    Ok(p) => {
                        s.front = Some(p);
                        s.front_apps = apps;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            (Mode::Tun, TunStack::Native) => self.start_native(&mut s, &nodes, settings.kill_switch && kill_switch_supported(&settings)).await,
        };
        if let Err(e) = res {
            self.teardown(s).await;
            return Err(e);
        }
        // Linux, own TUN: check the routing table itself — the helper may have hit something
        // (another VPN, IPv6 disabled on the interface) and the browser would quietly go around.
        #[cfg(target_os = "linux")]
        if tun && settings.tun_stack == TunStack::Native {
            let c = crate::linux_tun::route_check();
            self.logs.push(format!("[tun] routes: {}", c.summary()));
            if let Some(e) = c.leak() {
                self.logs.error(format!("[tun] {e}"));
                for l in crate::linux_tun::ip_snapshot() {
                    self.logs.push(format!("[tun] {l}"));
                }
                self.teardown(s).await;
                return Err(format!("TUN: {e}.\n\nNot connected, so that traffic does not leak. The routing table is in the log."));
            }
            if settings.kill_switch && !c.linkdown_ignored.is_empty() {
                self.logs.warn(format!(
                    "[killswitch] {} is set: if Mosaic is killed, the system may skip the blocked routes and traffic would leak",
                    c.linkdown_ignored.join(", ")
                ));
            }
        }
        // DNS leak protection is fail-closed: if it cannot be put in place, or the system's
        // DNS does not go through the tunnel with it, there is no connection — a VPN that
        // silently leaks DNS is worse than one that refuses to connect.
        if tun && settings.dns_leak_protection {
            self.step("dns", None);
            let checked = match self.protect_dns(&mut s) {
                Ok(()) => self.check_tunnel_dns().await,
                Err(e) => Err(e),
            };
            if let Err(e) = checked {
                self.teardown(s).await;
                return Err(format!(
                    "{e}\n\nNot connected, so that DNS does not leak. To connect without this protection, turn off \
                     “DNS leak protection” in Settings → TUN (not recommended)."
                ));
            }
        }
        // Kill switch: fail-closed like DNS protection — if it cannot be put in place, no connection.
        if settings.kill_switch && kill_switch_supported(&settings) {
            if let Err(e) = self.arm_kill_switch() {
                self.teardown(s).await;
                return Err(format!("{e}\n\nNot connected: the kill switch could not be enabled. Turn it off in Settings → TUN to connect without it."));
            }
        } else if self.kill_armed.load(Relaxed) {
            // Turned off (or no longer applicable). Windows: remove the filters. Linux: the new
            // helper runs without "ks" and owns the interface now — only the marker goes.
            #[cfg(windows)]
            self.release_kill_switch_os();
            #[cfg(not(windows))]
            {
                self.logs.push("[killswitch] off");
                self.set_kill_flag(false);
            }
        }
        self.stats.clear_closed();
        self.logs.push(match (tun, settings.webrtc_protection) {
            (true, true) => "[webrtc] protection on: STUN requests that rules send direct go through the node",
            (true, false) => "[webrtc] protection off: WebRTC in browsers can reveal the real IP",
            (false, true) => {
                "[webrtc] protection is on, but works only in TUN mode: with a proxy, browsers send WebRTC (UDP) directly, past Mosaic"
            }
            (false, false) => "[webrtc] protection off (and it works only in TUN mode)",
        });
        if settings.kill_switch && !tun && !cfg!(target_os = "android") {
            self.logs.warn("[killswitch] not active: it works only in TUN mode");
        }
        self.logs.push("[hub] connected");
        g.session = Some(s);
        Ok(())
    }

    fn set_kill_flag(&self, on: bool) {
        self.kill_armed.store(on, Relaxed);
        let path = self.dir.join(KILL_FLAG);
        if on {
            let _ = std::fs::write(&path, now_ms().to_string());
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Put the kill switch in force for the session that is coming up.
    fn arm_kill_switch(&self) -> Result<(), String> {
        #[cfg(windows)]
        {
            let mut apps: Vec<PathBuf> = EngineKind::ALL.iter().filter_map(|k| engines::find(*k)).collect();
            if let Ok(exe) = std::env::current_exe() {
                apps.push(exe);
            }
            let idx = crate::wfp::interface_index("Mosaic").ok();
            crate::wfp::killswitch_install(idx, &apps).map_err(|e| format!("Kill switch: {e}"))?;
        }
        // Linux: the helper was started with "ks" and keeps the interface if Mosaic dies.
        if !self.kill_armed.load(Relaxed) {
            self.logs.push("[killswitch] on: if the tunnel goes down unexpectedly, traffic is blocked");
        }
        self.set_kill_flag(true);
        Ok(())
    }

    /// Lift the block: filters (Windows) or the held interface (Linux) go away.
    fn release_kill_switch_os(&self) {
        #[cfg(windows)]
        if let Err(e) = crate::wfp::killswitch_remove() {
            self.logs.push(format!("[killswitch] could not remove the filters: {e}"));
            return;
        }
        #[cfg(target_os = "linux")]
        if std::path::Path::new("/sys/class/net/Mosaic").exists() {
            let res = crate::linux_tun::helper_binary(&engines::bundled_copy_dir()).and_then(|exe| {
                let out = std::process::Command::new(&exe).arg("clean").output().map_err(|e| e.to_string())?;
                if out.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&out.stdout).trim().trim_start_matches("error: ").to_string())
                }
            });
            if let Err(e) = res {
                self.logs.push(format!("[killswitch] could not remove the interface: {e}"));
                return;
            }
            crate::net::set_bypass_interface(0);
            crate::net::set_bypass_device("");
        }
        if self.kill_armed.load(Relaxed) {
            self.logs.push("[killswitch] off: traffic is no longer blocked");
        }
        self.set_kill_flag(false);
    }

    /// The user lifts the block while not connected ("Unblock the internet").
    pub async fn release_kill_switch(&self) -> Result<(), String> {
        let g = self.inner.lock().await;
        if g.session.is_some() {
            return Err("Mosaic is connected — disconnect to turn the kill switch off".into());
        }
        self.release_kill_switch_os();
        if self.kill_armed.load(Relaxed) {
            return Err("The block could not be lifted — see the log".into());
        }
        Ok(())
    }

    /// Block DNS that bypasses the tunnel (Windows WFP).
    #[cfg(windows)]
    fn protect_dns(&self, s: &mut Session) -> Result<(), String> {
        let mut apps: Vec<PathBuf> = EngineKind::ALL.iter().filter_map(|k| engines::find(*k)).collect();
        if let Ok(exe) = std::env::current_exe() {
            apps.push(exe);
        }
        let idx = crate::wfp::interface_index("Mosaic").map_err(|e| format!("DNS leak protection could not be enabled: {e}"))?;
        let g = crate::wfp::DnsGuard::install(Some(idx), &apps).map_err(|e| format!("DNS leak protection could not be enabled: {e}"))?;
        s.dns_guard = Some(g);
        self.logs.push("[hub] DNS leak protection on: DNS outside the tunnel is blocked");
        Ok(())
    }

    #[cfg(not(windows))]
    fn protect_dns(&self, _s: &mut Session) -> Result<(), String> {
        Ok(())
    }

    /// Ask the system resolver (as every app does) for a never-seen name. Through the tunnel
    /// the answer is a fake IP (198.18.0.0/15); a real answer or none means DNS goes elsewhere
    /// or does not work. Windows only: there Mosaic's own lookups also use the system DNS
    /// service, so the test sees exactly what apps see.
    #[cfg(windows)]
    async fn check_tunnel_dns(&self) -> Result<(), String> {
        let name = format!("mosaic-dns-check-{}.example.com", now_ms());
        let mut last = String::new();
        for _ in 0..4 {
            match tokio::time::timeout(Duration::from_secs(4), tokio::net::lookup_host((name.as_str(), 80))).await {
                Ok(Ok(addrs)) => {
                    let ips: Vec<std::net::IpAddr> = addrs.map(|a| a.ip()).collect();
                    let fake = |ip: &std::net::IpAddr| matches!(ip, std::net::IpAddr::V4(v4) if (u32::from(*v4) & 0xFFFE_0000) == 0xC612_0000);
                    if ips.iter().any(fake) {
                        self.logs.push("[hub] DNS check passed: system DNS goes through the tunnel");
                        return Ok(());
                    }
                    if let Some(ip) = ips.first() {
                        return Err(format!("DNS check failed: the system DNS answered from outside the tunnel ({ip}) — DNS would leak."));
                    }
                    last = "empty answer".into();
                }
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => last = "no answer within 4 s".into(),
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        self.log_dns_setup();
        if let Some(hint) = nrpt_override() {
            return Err(format!("DNS check failed: {hint}"));
        }
        Err(format!("DNS check failed: names cannot be resolved in TUN mode ({last})."))
    }

    /// For the logs when the DNS check fails: interface order and DNS servers as Windows sees them.
    #[cfg(windows)]
    fn log_dns_setup(&self) {
        use std::os::windows::process::CommandExt;
        let script = "Get-NetIPInterface -AddressFamily IPv4 | Sort-Object InterfaceMetric | Select-Object -First 6 InterfaceAlias,InterfaceMetric,ConnectionState | Format-Table -AutoSize | Out-String -Width 200; \
                      Get-DnsClientServerAddress -AddressFamily IPv4 | Where-Object { $_.ServerAddresses } | Format-Table InterfaceAlias,ServerAddresses -AutoSize | Out-String -Width 200; \
                      Get-NetRoute -AddressFamily IPv4 -DestinationPrefix 0.0.0.0/0 | Format-Table InterfaceAlias,NextHop,RouteMetric -AutoSize | Out-String -Width 200; \
                      Get-DnsClientNrptPolicy | Format-Table Namespace,NameServers -AutoSize | Out-String -Width 200";
        if let Ok(out) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(0x0800_0000)
            .output()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()) {
                self.logs.push(format!("[dns-check] {line}"));
            }
        }
    }

    #[cfg(not(windows))]
    async fn check_tunnel_dns(&self) -> Result<(), String> {
        Ok(())
    }

    #[cfg(feature = "native-tun")]
    async fn start_native(&self, s: &mut Session, nodes: &[Node], st_kill_switch: bool) -> Result<(), String> {
        let params = crate::tun_native::NativeParams {
            server_domains: server_domains(nodes),
            // Linux: the engines got their servers as IPs (pinned); the direct DNS server is not
            // bypassed — the hub's own sockets are bound to the physical interface anyway, and
            // apps must not reach it around the tunnel.
            server_ips: if cfg!(target_os = "linux") {
                let dns = s.resolver.server().map(|a| a.ip());
                s.server_ips.iter().copied().filter(|ip| Some(*ip) != dns).collect()
            } else {
                nodes.iter().filter_map(|n| n.server.parse().ok()).collect()
            },
            resolver: s.resolver.clone(),
            fd: self.tun_fd.lock().unwrap().take(),
            #[cfg(target_os = "linux")]
            helper: Some(crate::linux_tun::helper_binary(&engines::bundled_copy_dir())?),
            #[cfg(not(target_os = "linux"))]
            helper: None,
            kill_switch: st_kill_switch,
            routing: s.hub.shared(),
            stats: self.stats.clone(),
            logs: self.logs.clone(),
        };
        let t = crate::tun_native::NativeTun::start(params).await?;
        s.native = Some(t);
        Ok(())
    }

    #[cfg(not(feature = "native-tun"))]
    async fn start_native(&self, _s: &mut Session, _nodes: &[Node], _kill_switch: bool) -> Result<(), String> {
        Err("This build has no native TUN stack".into())
    }

    async fn teardown(&self, s: Session) {
        #[cfg(feature = "native-tun")]
        let native = s.native;
        let hub = s.hub;
        let engines = s.engines;
        let front = s.front;
        let system_proxy = s.system_proxy;
        #[cfg(windows)]
        let dns_guard = s.dns_guard;
        if system_proxy {
            if let Err(e) = sys::unset_system_proxy() {
                self.logs.push(format!("[hub] could not disable the system proxy: {e}"));
            }
        }
        #[cfg(feature = "native-tun")]
        if let Some(t) = native {
            // With the kill switch in force the interface stays and keeps taking the traffic.
            t.stop(self.kill_armed.load(Relaxed)).await;
        }
        if let Some(f) = front {
            f.stop().await;
        }
        hub.stop();
        for p in engines {
            p.stop().await;
        }
        // Filters go last so DNS stays inside the tunnel until it is gone.
        #[cfg(windows)]
        drop(dns_guard);
    }

    /// The user disconnects (or quits): the kill switch is lifted as well.
    pub async fn disconnect(&self) {
        let armed = self.kill_armed.load(Relaxed);
        if armed {
            // The helper removes the interface instead of holding it.
            self.kill_armed.store(false, Relaxed);
        }
        self.drop_session().await;
        if armed {
            self.kill_armed.store(true, Relaxed);
            self.release_kill_switch_os();
        }
    }

    /// End the session for a reconnect: the kill switch (if any) stays in force meanwhile.
    async fn drop_session(&self) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.session.take() {
            self.teardown(s).await;
            self.logs.push("[hub] disconnected");
        }
        self.set_health("unknown", None, None);
        if g.state.proxy_dirty {
            g.state.proxy_dirty = false;
            self.save(&g.state);
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.inner.lock().await.session.is_some()
    }

    // ------------------------------------------------------------ latency

    /// Test every node through its engine. Starts temporary engines if not connected.
    pub async fn test_latency(&self) -> Result<(), String> {
        let (nodes, url, existing) = {
            let g = self.inner.lock().await;
            (g.state.nodes.clone(), g.state.settings.test_url.clone(), g.session.as_ref().map(|s| s.upstreams.clone()))
        };
        if nodes.is_empty() {
            return Ok(());
        }
        let (temp, upstreams) = match existing {
            Some(u) => (Vec::new(), u),
            None => self.start_engines(&nodes).await?,
        };
        let mut set = JoinSet::new();
        for n in &nodes {
            if let Some(u) = upstreams.get(&n.id) {
                let (id, port, url) = (n.id.clone(), u.port, url.clone());
                set.spawn(async move {
                    let r = hub::url_probe(port, &url).await;
                    (id, r)
                });
            }
        }
        let mut results: Vec<(String, Result<(u32, Option<u32>), String>)> = Vec::new();
        while let Some(r) = set.join_next().await {
            if let Ok(x) = r {
                results.push(x);
            }
        }
        for p in temp {
            p.stop().await;
        }
        let now = now_ms();
        let mut g = self.inner.lock().await;
        for n in &nodes {
            if !upstreams.contains_key(&n.id) {
                let why = n.effective_engine().err().unwrap_or_else(|| "engine did not start".into());
                g.latency.insert(n.id.clone(), Latency { ms: None, rtt: None, error: Some(why), at_ms: now });
            }
        }
        for (id, r) in results {
            let l = match r {
                Ok((ms, rtt)) => Latency { ms: Some(ms), rtt, error: None, at_ms: now },
                Err(e) => Latency { ms: None, rtt: None, error: Some(e), at_ms: now },
            };
            g.latency.insert(id, l);
        }
        Ok(())
    }
}
