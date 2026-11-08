//! The controller owns state, the running session (hub + engines) and is the
//! only API the UI and CLI talk to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::engines::{self, singbox, Slot};
use crate::hub::{self, Hub, Routing, Upstream};
use crate::links;
use crate::model::*;
use crate::process::{self, EngineProcess};
use crate::stats::{ConnView, Stats};
use crate::store;
use crate::sys;
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
    pub since_ms: Option<u64>,
    pub up: u64,
    pub down: u64,
    pub active_conns: usize,
    pub listen_port: Option<u16>,
    pub system_proxy: bool,
    pub tun: bool,
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
    pub nodes: Vec<NodeView>,
    pub subscriptions: Vec<Subscription>,
    pub rules: Vec<Rule>,
    pub settings: Settings,
    pub selected: Option<String>,
    pub status: Status,
    pub engines: Vec<EngineInfo>,
    pub is_admin: bool,
    pub data_dir: String,
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
    front: Option<EngineProcess>,
    upstreams: HashMap<String, Upstream>,
    mode: Mode,
    system_proxy: bool,
    started_ms: u64,
    /// TUN mode: SOCKS port of the TUN adapter used for "direct" traffic.
    direct_port: Option<u16>,
    /// Node server domains (resolved with real DNS inside the TUN adapter).
    server_domains: Vec<String>,
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
    dir: PathBuf,
    admin: bool,
}

fn routing_for(st: &State, upstreams: &HashMap<String, Upstream>, direct_via: Option<u16>) -> Routing {
    Routing {
        rules: st.rules.clone(),
        final_target: st.settings.final_target.clone(),
        selected: st.selected.clone(),
        upstreams: upstreams.clone(),
        direct_via,
    }
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
                Ok(()) => logs.push("[hub] системный прокси сброшен после прошлого некорректного завершения"),
                Err(e) => logs.push(format!("[hub] не удалось сбросить системный прокси: {e}")),
            }
            state.proxy_dirty = false;
            let _ = store::save_state(&dir, &state);
        }
        logs.push(format!("[hub] данные: {}", dir.display()));
        Self {
            inner: Mutex::new(Inner { state, session: None, latency: HashMap::new() }),
            stats: Arc::new(Stats::default()),
            logs,
            dir,
            admin: sys::is_admin(),
        }
    }

    fn save(&self, st: &State) {
        if let Err(e) = store::save_state(&self.dir, st) {
            self.logs.push(format!("[hub] не удалось сохранить настройки: {e}"));
        }
    }

    fn write_config(&self, name: &str, cfg: &Value) -> Result<PathBuf, String> {
        let dir = self.dir.join("run");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{name}.json"));
        let bytes = serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?;
        std::fs::write(&path, bytes).map_err(|e| format!("не удалось записать {}: {e}", path.display()))?;
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
            since_ms: s.map(|s| s.started_ms),
            up: self.stats.up.load(Relaxed),
            down: self.stats.down.load(Relaxed),
            active_conns: self.stats.active_count(),
            listen_port: s.map(|s| s.hub.port),
            system_proxy: s.map_or(false, |s| s.system_proxy),
            tun: s.map_or(false, |s| s.front.is_some()),
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
        Snapshot {
            nodes,
            subscriptions: st.subscriptions.clone(),
            rules: st.rules.clone(),
            settings: st.settings.clone(),
            selected: st.selected.clone(),
            status,
            engines: engine_infos,
            is_admin: self.admin,
            data_dir: self.dir.display().to_string(),
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
            self.logs.push("[hub] новые узлы станут доступны после переподключения");
        }
        ImportSummary { added, updated, errors: res.errors }
    }

    pub async fn add_subscription(&self, url: &str) -> Result<ImportSummary, String> {
        let url = url.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("Ссылка подписки должна начинаться с http:// или https://".into());
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
                return Err(format!("Не удалось загрузить подписку: {e}"));
            }
        };
        let res = links::parse_text(&body);
        if res.nodes.is_empty() {
            let why = res.errors.first().cloned().unwrap_or_else(|| "пустой ответ".into());
            if let Some(s) = st.subscriptions.iter_mut().find(|s| s.url == url) {
                s.last_error = Some(why.clone());
            }
            self.save(st);
            return Err(format!("В подписке не найдено поддерживаемых узлов ({why})"));
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

    pub async fn select_node(&self, id: &str) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        let name = g.state.nodes.iter().find(|n| n.id == id).map(|n| n.name.clone()).ok_or("узел не найден")?;
        g.state.selected = Some(id.to_string());
        self.save(&g.state);
        let inner = &mut *g;
        if let Some(s) = inner.session.as_mut() {
            if !s.upstreams.contains_key(id) {
                return Err("Этот узел не запущен — переподключитесь".into());
            }
            // Instant switch: just swap the routing table.
            s.hub.update(routing_for(&inner.state, &s.upstreams, s.direct_port));
            self.logs.push(format!("[hub] активный узел → {name}"));
            if s.mode == Mode::Tun {
                // UDP goes straight from the TUN adapter to the node's engine.
                self.restart_front(s, &inner.state.settings, Some(id)).await?;
            }
        }
        Ok(())
    }

    pub async fn set_node_engine(&self, id: &str, choice: EngineChoice) -> Result<(), String> {
        let running = {
            let mut g = self.inner.lock().await;
            let n = g.state.nodes.iter_mut().find(|n| n.id == id).ok_or("узел не найден")?;
            let mut probe = n.clone();
            probe.engine = choice;
            probe.effective_engine()?;
            n.engine = choice;
            self.save(&g.state);
            g.session.is_some()
        };
        if running {
            self.logs.push("[hub] ядро узла изменено — перезапуск движков");
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
        let inner = &mut *g;
        if let Some(s) = &inner.session {
            s.hub.update(routing_for(&inner.state, &s.upstreams, s.direct_port));
            self.logs.push("[hub] правила применены");
        }
    }

    pub async fn set_settings(&self, mut new: Settings) -> Result<(), String> {
        if new.listen_port < 1024 {
            new.listen_port = 7890;
        }
        let restart = {
            let mut g = self.inner.lock().await;
            let old = g.state.settings.clone();
            g.state.settings = new.clone();
            self.save(&g.state);
            let inner = &mut *g;
            if let Some(s) = &inner.session {
                s.hub.update(routing_for(&inner.state, &s.upstreams, s.direct_port));
            }
            inner.session.is_some()
                && (old.mode != new.mode || old.listen_port != new.listen_port || old.block_quic != new.block_quic)
        };
        if restart {
            self.disconnect().await;
            self.connect().await?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ session

    async fn spawn_engine(&self, kind: EngineKind, slots: &[Slot]) -> Result<EngineProcess, String> {
        let exe = engines::find(kind).ok_or_else(|| {
            format!("{} не найден: положите {} в папку engines (scripts\\get-engines.ps1)", kind.label(), kind.exe_name())
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
                Err(e) => self.logs.push(format!("[hub] «{}» пропущен: {e}", n.name)),
            }
        }
        if plan.is_empty() {
            return Err("Ни один узел не поддерживается доступными ядрами".into());
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
        udp_port: Option<u16>,
        domains: &[String],
        settings: &Settings,
    ) -> Result<EngineProcess, String> {
        let exe = engines::find(EngineKind::Singbox).ok_or("Для TUN-режима нужен sing-box в папке engines")?;
        let mut procs: Vec<String> = vec![EngineKind::Xray.exe_name().into(), EngineKind::Singbox.exe_name().into()];
        if let Some(name) = std::env::current_exe().ok().and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())) {
            procs.push(name);
        }
        let dns = if settings.direct_dns.trim().is_empty() { "8.8.8.8" } else { settings.direct_dns.trim() };
        let cfg = singbox::compile_tun(hub_port, direct_port, udp_port, &procs, domains, dns, settings.block_quic);
        let path = self.write_config("tun", &cfg)?;
        let mut p = EngineProcess::spawn(EngineKind::Singbox, "tun", &exe, &path, self.logs.clone()).await?;
        if let Err(e) = p.wait_ports(&[direct_port], Duration::from_secs(8)).await {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let msg = self.engine_error(format!("TUN-адаптер не запустился: {e}"), "sing-box/tun");
            p.stop().await;
            return Err(msg);
        }
        // Give the interface and routes a moment, fail fast if it dies.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(st) = p.exited() {
                tokio::time::sleep(Duration::from_millis(200)).await;
                return Err(self.engine_error(format!("TUN-адаптер завершился ({st})"), "sing-box/tun"));
            }
        }
        self.logs.push("[hub] TUN-интерфейс «Mosaic» поднят");
        Ok(p)
    }

    async fn restart_front(&self, s: &mut Session, settings: &Settings, selected: Option<&str>) -> Result<(), String> {
        if let Some(old) = s.front.take() {
            old.stop().await;
        }
        let Some(direct_port) = s.direct_port else { return Ok(()) };
        let udp = selected.and_then(|id| s.upstreams.get(id)).map(|u| u.port);
        let p = self.start_front(s.hub.port, direct_port, udp, &s.server_domains, settings).await?;
        s.front = Some(p);
        Ok(())
    }

    pub async fn connect(&self) -> Result<(), String> {
        let mut g = self.inner.lock().await;
        if g.session.is_some() {
            return Ok(());
        }
        if g.state.nodes.is_empty() {
            return Err("Сначала добавьте узел или подписку".into());
        }
        fix_selection(&mut g.state);
        let settings = g.state.settings.clone();
        if settings.mode == Mode::Tun && !self.admin {
            return Err("TUN-режим требует запуска Mosaic от имени администратора".into());
        }
        self.logs.push("[hub] подключение…");
        let nodes = g.state.nodes.clone();
        let (engine_procs, upstreams) = self.start_engines(&nodes).await?;
        let selected = g.state.selected.clone();
        let direct_port = if settings.mode == Mode::Tun {
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
        let routing = routing_for(&g.state, &upstreams, direct_port);
        let hub = match Hub::start(settings.listen_port, routing, self.stats.clone(), self.logs.clone()).await {
            Ok(h) => h,
            Err(e) => {
                for p in engine_procs {
                    p.stop().await;
                }
                return Err(e);
            }
        };
        let mut s = Session {
            hub,
            engines: engine_procs,
            front: None,
            upstreams,
            mode: settings.mode,
            system_proxy: false,
            started_ms: now_ms(),
            direct_port,
            server_domains: server_domains(&nodes),
        };
        if let Some(id) = &selected {
            if !s.upstreams.contains_key(id) {
                self.logs.push("[hub] выбранный узел не запустился — трафик через «proxy» будет блокироваться");
            }
        }
        let res: Result<(), String> = match settings.mode {
            Mode::SystemProxy => match sys::set_system_proxy(settings.listen_port) {
                Ok(true) => {
                    s.system_proxy = true;
                    g.state.proxy_dirty = true;
                    self.save(&g.state);
                    self.logs.push(format!("[hub] системный прокси → 127.0.0.1:{}", settings.listen_port));
                    Ok(())
                }
                Ok(false) => {
                    self.logs.push(format!(
                        "[hub] на этой ОС системный прокси не настраивается автоматически; используйте 127.0.0.1:{}",
                        settings.listen_port
                    ));
                    Ok(())
                }
                Err(e) => Err(format!("Не удалось включить системный прокси: {e}")),
            },
            Mode::Tun => {
                let udp = selected.as_ref().and_then(|id| s.upstreams.get(id)).map(|u| u.port);
                let dp = direct_port.unwrap_or(0);
                match self.start_front(s.hub.port, dp, udp, &s.server_domains, &settings).await {
                    Ok(p) => {
                        s.front = Some(p);
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
        };
        if let Err(e) = res {
            self.teardown(s).await;
            return Err(e);
        }
        self.stats.clear_closed();
        self.logs.push("[hub] подключено");
        g.session = Some(s);
        Ok(())
    }

    async fn teardown(&self, s: Session) {
        let Session { hub, engines, front, system_proxy, .. } = s;
        if system_proxy {
            if let Err(e) = sys::unset_system_proxy() {
                self.logs.push(format!("[hub] не удалось выключить системный прокси: {e}"));
            }
        }
        if let Some(f) = front {
            f.stop().await;
        }
        hub.stop();
        for p in engines {
            p.stop().await;
        }
    }

    pub async fn disconnect(&self) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.session.take() {
            self.teardown(s).await;
            self.logs.push("[hub] отключено");
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
                let why = n.effective_engine().err().unwrap_or_else(|| "ядро не запустилось".into());
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
