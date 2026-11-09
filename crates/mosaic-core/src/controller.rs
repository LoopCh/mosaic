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
    pub ms: Option<u32>,
    pub error: Option<String>,
    pub at_ms: u64,
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
    geo: Arc<GeoData>,
    resolver: Arc<Resolver>,
    remote_cache: Arc<DnsCache>,
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
    app_update: std::sync::Mutex<Option<AppUpdate>>,
    /// Android: the VPN interface handed over by VpnService for the next TUN connect.
    tun_fd: std::sync::Mutex<Option<i32>>,
    dir: PathBuf,
    admin: bool,
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

async fn fetch(url: String, ua: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let resp = ureq::get(&url)
            .set("User-Agent", &ua)
            .timeout(Duration::from_secs(20))
            .call()
            .map_err(|e| e.to_string())?;
        resp.into_string().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

impl Controller {
    pub fn load() -> Self {
        let dir = store::data_dir();
        let _ = std::fs::create_dir_all(dir.join("run"));
        let mut state = store::load_state(&dir);
        let logs = Arc::new(LogBuf::new(2000));
        if state.proxy_dirty {
            match sys::unset_system_proxy() {
                Ok(()) => logs.push("[hub] system proxy reset after an unclean shutdown"),
                Err(e) => logs.push(format!("[hub] could not reset the system proxy: {e}")),
            }
            state.proxy_dirty = false;
            let _ = store::save_state(&dir, &state);
        }
        logs.push(format!("[hub] data: {}", dir.display()));
        let close_to_tray = AtomicBool::new(state.settings.close_to_tray);
        Self {
            inner: Mutex::new(Inner { state, session: None, latency: HashMap::new() }),
            stats: Arc::new(Stats::default()),
            logs,
            close_to_tray,
            smart_kick: AtomicBool::new(true),
            app_update: std::sync::Mutex::new(None),
            tun_fd: std::sync::Mutex::new(None),
            dir,
            // Android: the VPN permission dialog replaces administrator rights.
            admin: sys::is_admin() || cfg!(target_os = "android"),
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
            native_tun_available: cfg!(feature = "native-tun"),
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
                g.state.subscriptions.push(Subscription { url: url.clone(), name: host_of(&url), updated_ms: 0, last_error: None });
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
        let body = match fetched {
            Ok(b) => b,
            Err(e) => {
                if let Some(s) = st.subscriptions.iter_mut().find(|s| s.url == url) {
                    s.last_error = Some(e.clone());
                }
                self.save(st);
                return Err(format!("Could not download the subscription: {e}"));
            }
        };
        let res = links::parse_text(&body);
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
            self.disconnect().await;
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
            // The sing-box adapter carries the per-app rules: rebuild it.
            if let (Some(old), Some(dp)) = (s.front.take(), s.direct_port) {
                old.stop().await;
                let apps = app_rules(&inner.state, s);
                match self.start_front(s.hub.port, dp, &s.server_domains, &inner.state.settings, &apps).await {
                    Ok(p) => s.front = Some(p),
                    Err(e) => self.logs.push(format!("[hub] TUN adapter restart failed: {e}")),
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
            self.disconnect().await;
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
                self.disconnect().await;
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
            self.disconnect().await;
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
            if settings.check_updates && (tick == 3 || tick % 8640 == 0) {
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
                fails = 0;
                last_full = None;
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
                    }
                    Err(_) => *fails += 1,
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
            set.spawn(async move { (id, hub::url_test(port, &url).await) });
        }
        let mut results: Vec<(String, Result<u32, String>)> = Vec::new();
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
                    Ok(ms) => Latency { ms: Some(*ms), error: None, at_ms: now },
                    Err(e) => Latency { ms: None, error: Some(e.clone()), at_ms: now },
                };
                g.latency.insert(id.clone(), l);
            }
        }
        let best = results.iter().filter_map(|(id, r)| r.as_ref().ok().map(|ms| (id.clone(), *ms))).min_by_key(|(_, ms)| *ms);
        let Some((best_id, best_ms)) = best else {
            self.logs.push("[smart] no node answered the test");
            return;
        };
        let current = selected.as_ref().and_then(|id| results.iter().find(|(i, _)| i == id)).and_then(|(_, r)| r.as_ref().ok().copied());
        let current = current.or(current_ms);
        let switch = match current {
            None => true,                                                   // active node is down
            Some(cur) => *fails >= 2 || cur > best_ms + best_ms * 3 / 10 + 50, // clearly faster
        };
        if switch && selected.as_deref() != Some(best_id.as_str()) {
            let name = upstreams.get(&best_id).map(|u| u.name.clone()).unwrap_or_default();
            match self.select_inner(&best_id, false).await {
                Ok(()) => self.logs.push(format!("[smart] switched to {name} ({best_ms} ms)")),
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
                    || (new.mode == Mode::Tun && old.remote_dns != new.remote_dns && new.tun_stack == TunStack::Singbox))
        };
        if restart {
            self.disconnect().await;
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
            format!("{} not found: put {} into the engines folder (scripts\\get-engines.ps1)", kind.label(), kind.exe_name())
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
            upstreams.insert(n.id.clone(), Upstream { port, name: n.name.clone(), engine: k });
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
        settings: &Settings,
        apps: &[(Vec<String>, singbox::FrontTarget)],
    ) -> Result<EngineProcess, String> {
        let exe = engines::find(EngineKind::Singbox).ok_or("TUN mode needs sing-box in the engines folder")?;
        let mut procs: Vec<String> = vec![EngineKind::Xray.exe_name().into(), EngineKind::Singbox.exe_name().into()];
        if let Some(name) = std::env::current_exe().ok().and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())) {
            procs.push(name);
        }
        let dns = if settings.direct_dns.trim().is_empty() { "8.8.8.8" } else { settings.direct_dns.trim() };
        let dns = dns.split(':').next().unwrap_or("8.8.8.8");
        let remote = resolver::parse_dns_server(&settings.remote_dns, "1.1.1.1:53");
        let cfg = singbox::compile_tun(hub_port, direct_port, &procs, domains, dns, &remote.ip().to_string(), settings.block_quic, apps);
        let path = self.write_config("tun", &cfg)?;
        let mut p = EngineProcess::spawn(EngineKind::Singbox, "tun", &exe, &path, self.logs.clone()).await?;
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
        self.logs.push("[hub] TUN interface 'Mosaic' is up (sing-box adapter)");
        Ok(p)
    }

    pub async fn connect(&self) -> Result<(), String> {
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
        if tun && !self.admin {
            return Err("TUN mode requires running Mosaic as administrator".into());
        }
        if tun && settings.tun_stack == TunStack::Native && !cfg!(feature = "native-tun") {
            return Err("This build has no native TUN stack. Switch the TUN stack to sing-box in Settings, or use the build with the native-tun feature.".into());
        }
        self.logs.push("[hub] connecting…");
        let nodes = g.state.nodes.clone();
        let engine_nodes =
            if cfg!(target_os = "android") { self.pin_servers(nodes.clone(), &settings.direct_dns).await } else { nodes.clone() };
        let (engine_procs, upstreams) = self.start_engines(&engine_nodes).await?;
        let selected = g.state.selected.clone();
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
            geo,
            resolver,
            remote_cache: Arc::new(DnsCache::default()),
            #[cfg(windows)]
            dns_guard: None,
        };
        let mut s = draft;
        s.hub.update(routing_for(&g.state, &s));
        if let Some(id) = &selected {
            if !s.upstreams.contains_key(id) {
                self.logs.push("[hub] the selected node did not start — 'proxy' traffic will be blocked");
            }
        }
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
                match self.start_front(s.hub.port, dp, &s.server_domains, &settings, &apps).await {
                    Ok(p) => {
                        s.front = Some(p);
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            (Mode::Tun, TunStack::Native) => self.start_native(&mut s, &nodes).await,
        };
        if let Err(e) = res {
            self.teardown(s).await;
            return Err(e);
        }
        if tun && settings.dns_leak_protection {
            self.protect_dns(&mut s);
        }
        self.stats.clear_closed();
        self.logs.push("[hub] connected");
        g.session = Some(s);
        Ok(())
    }

    /// Block DNS that bypasses the tunnel (Windows WFP). Failure is logged, not fatal.
    #[cfg(windows)]
    fn protect_dns(&self, s: &mut Session) {
        let mut apps: Vec<PathBuf> = EngineKind::ALL.iter().filter_map(|k| engines::find(*k)).collect();
        if let Ok(exe) = std::env::current_exe() {
            apps.push(exe);
        }
        let res = crate::wfp::interface_index("Mosaic").and_then(|idx| crate::wfp::DnsGuard::install(idx, &apps));
        match res {
            Ok(g) => {
                s.dns_guard = Some(g);
                self.logs.push("[hub] DNS leak protection on: DNS outside the tunnel is blocked");
            }
            Err(e) => self.logs.push(format!("[hub] DNS leak protection could not be enabled: {e}")),
        }
    }

    #[cfg(not(windows))]
    fn protect_dns(&self, _s: &mut Session) {}

    #[cfg(feature = "native-tun")]
    async fn start_native(&self, s: &mut Session, nodes: &[Node]) -> Result<(), String> {
        let params = crate::tun_native::NativeParams {
            server_domains: server_domains(nodes),
            server_ips: nodes.iter().filter_map(|n| n.server.parse().ok()).collect(),
            resolver: s.resolver.clone(),
            fd: self.tun_fd.lock().unwrap().take(),
            routing: s.hub.shared(),
            stats: self.stats.clone(),
            logs: self.logs.clone(),
        };
        let t = crate::tun_native::NativeTun::start(params).await?;
        s.native = Some(t);
        Ok(())
    }

    #[cfg(not(feature = "native-tun"))]
    async fn start_native(&self, _s: &mut Session, _nodes: &[Node]) -> Result<(), String> {
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
            t.stop().await;
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

    pub async fn disconnect(&self) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.session.take() {
            self.teardown(s).await;
            self.logs.push("[hub] disconnected");
        }
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
                    let r = hub::url_test(port, &url).await;
                    (id, r)
                });
            }
        }
        let mut results: Vec<(String, Result<u32, String>)> = Vec::new();
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
                g.latency.insert(n.id.clone(), Latency { ms: None, error: Some(why), at_ms: now });
            }
        }
        for (id, r) in results {
            let l = match r {
                Ok(ms) => Latency { ms: Some(ms), error: None, at_ms: now },
                Err(e) => Latency { ms: None, error: Some(e), at_ms: now },
            };
            g.latency.insert(id, l);
        }
        Ok(())
    }
}
