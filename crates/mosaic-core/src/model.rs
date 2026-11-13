//! The unified configuration model (IR). Nothing in here knows about any
//! particular core; engine configs are compiled from these types.

use serde::{Deserialize, Serialize};

use crate::engines;

// ---------------------------------------------------------------- engines

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    Xray,
    Singbox,
}

impl EngineKind {
    pub const ALL: [EngineKind; 2] = [EngineKind::Xray, EngineKind::Singbox];

    pub fn label(self) -> &'static str {
        match self {
            EngineKind::Xray => "Xray",
            EngineKind::Singbox => "sing-box",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            EngineKind::Xray => "xray",
            EngineKind::Singbox => "singbox",
        }
    }

    pub fn exe_name(self) -> &'static str {
        // Android only lets apps execute files from their native library folder,
        // so the engines are packaged as lib*.so there.
        if cfg!(target_os = "android") {
            return match self {
                EngineKind::Xray => "libxray.so",
                EngineKind::Singbox => "libsingbox.so",
            };
        }
        match (self, cfg!(windows)) {
            (EngineKind::Xray, true) => "xray.exe",
            (EngineKind::Xray, false) => "xray",
            (EngineKind::Singbox, true) => "sing-box.exe",
            (EngineKind::Singbox, false) => "sing-box",
        }
    }
}

/// What the user picked for a node. `Auto` consults the capability matrix.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum EngineChoice {
    #[default]
    Auto,
    Xray,
    Singbox,
}

// ---------------------------------------------------------------- nodes

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Protocol {
    Vless {
        uuid: String,
        #[serde(default)]
        flow: String,
    },
    Vmess {
        uuid: String,
        #[serde(default)]
        alter_id: u32,
        /// "auto", "aes-128-gcm", "chacha20-poly1305", "none" …; empty = auto
        #[serde(default)]
        cipher: String,
    },
    Trojan {
        password: String,
    },
    Shadowsocks {
        method: String,
        password: String,
    },
    Hysteria2 {
        password: String,
        #[serde(default)]
        obfs_password: String,
        #[serde(default)]
        up_mbps: u32,
        #[serde(default)]
        down_mbps: u32,
    },
    Tuic {
        uuid: String,
        password: String,
        #[serde(default)]
        congestion: String,
        #[serde(default)]
        udp_relay_mode: String,
    },
    Wireguard {
        private_key: String,
        peer_public_key: String,
        #[serde(default)]
        pre_shared_key: String,
        #[serde(default)]
        local_address: Vec<String>,
        #[serde(default)]
        mtu: u32,
        #[serde(default)]
        reserved: Vec<u8>,
    },
    /// A plain SOCKS5 proxy (optionally with a login). Carries TCP and UDP. Not encrypted:
    /// the traffic to the proxy is as visible as without it (sites themselves use HTTPS).
    Socks {
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    },
    /// An HTTP proxy (CONNECT), optionally over TLS (an "HTTPS proxy") and with a login.
    /// TCP only: UDP (QUIC, calls) sent to it is refused so that apps fall back to TCP.
    Http {
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    },
}

impl Protocol {
    /// Can the node carry UDP at all?
    pub fn udp(&self) -> bool {
        !matches!(self, Protocol::Http { .. })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Protocol::Vless { .. } => "VLESS",
            Protocol::Vmess { .. } => "VMess",
            Protocol::Trojan { .. } => "Trojan",
            Protocol::Shadowsocks { .. } => "Shadowsocks",
            Protocol::Hysteria2 { .. } => "Hysteria2",
            Protocol::Tuic { .. } => "TUIC",
            Protocol::Wireguard { .. } => "WireGuard",
            Protocol::Socks { .. } => "SOCKS5",
            Protocol::Http { .. } => "HTTP",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Transport {
    #[default]
    Tcp,
    Ws {
        path: String,
        host: String,
    },
    Grpc {
        service_name: String,
    },
    Httpupgrade {
        path: String,
        host: String,
    },
    Xhttp {
        path: String,
        host: String,
        #[serde(default)]
        mode: String,
    },
}

impl Transport {
    pub fn name(&self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Ws { .. } => "ws",
            Transport::Grpc { .. } => "grpc",
            Transport::Httpupgrade { .. } => "httpupgrade",
            Transport::Xhttp { .. } => "xhttp",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Security {
    #[default]
    None,
    Tls {
        #[serde(default)]
        sni: String,
        #[serde(default)]
        alpn: Vec<String>,
        #[serde(default)]
        fingerprint: String,
        #[serde(default)]
        insecure: bool,
    },
    Reality {
        #[serde(default)]
        sni: String,
        public_key: String,
        #[serde(default)]
        short_id: String,
        #[serde(default)]
        fingerprint: String,
        #[serde(default)]
        spider_x: String,
    },
}

impl Security {
    pub fn name(&self) -> &'static str {
        match self {
            Security::None => "none",
            Security::Tls { .. } => "tls",
            Security::Reality { .. } => "reality",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub server: String,
    pub port: u16,
    pub protocol: Protocol,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub security: Security,
    #[serde(default)]
    pub engine: EngineChoice,
    /// Subscription URL this node came from, if any.
    #[serde(default)]
    pub source: Option<String>,
    /// Original share link (for re-export / QR).
    #[serde(default)]
    pub link: String,
}

impl Node {
    pub fn summary(&self) -> String {
        match &self.protocol {
            Protocol::Hysteria2 { .. } | Protocol::Tuic { .. } => format!("{} · quic", self.protocol.name()),
            Protocol::Wireguard { .. } | Protocol::Shadowsocks { .. } => self.protocol.name().to_string(),
            Protocol::Socks { username, .. } | Protocol::Http { username, .. } => {
                let name = match (&self.protocol, &self.security) {
                    (Protocol::Http { .. }, Security::Tls { .. }) => "HTTPS",
                    _ => self.protocol.name(),
                };
                if username.is_empty() {
                    format!("{name} proxy")
                } else {
                    format!("{name} proxy · login")
                }
            }
            _ => format!("{} · {} · {}", self.protocol.name(), self.transport.name(), self.security.name()),
        }
    }

    pub fn supports(&self, kind: EngineKind) -> bool {
        match kind {
            EngineKind::Xray => engines::xray::supports(self),
            EngineKind::Singbox => engines::singbox::supports(self),
        }
    }

    pub fn supported_engines(&self) -> Vec<EngineKind> {
        EngineKind::ALL.into_iter().filter(|k| self.supports(*k)).collect()
    }

    /// Resolve the engine for this node: explicit choice if supported,
    /// otherwise the capability matrix's preference.
    pub fn effective_engine(&self) -> Result<EngineKind, String> {
        let wanted = match self.engine {
            EngineChoice::Xray => Some(EngineKind::Xray),
            EngineChoice::Singbox => Some(EngineKind::Singbox),
            EngineChoice::Auto => None,
        };
        if let Some(k) = wanted {
            return if self.supports(k) {
                Ok(k)
            } else {
                Err(format!("{} does not support {}", k.label(), self.summary()))
            };
        }
        // Xray is the reference implementation for the V2Ray family;
        // sing-box covers QUIC-based protocols and WireGuard.
        let order = match self.protocol {
            Protocol::Vless { .. } | Protocol::Vmess { .. } | Protocol::Trojan { .. } => {
                [EngineKind::Xray, EngineKind::Singbox]
            }
            _ => [EngineKind::Singbox, EngineKind::Xray],
        };
        order
            .into_iter()
            .find(|k| self.supports(*k))
            .ok_or_else(|| format!("no engine supports {}", self.summary()))
    }
}

// ---------------------------------------------------------------- rules

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Domain,
    DomainSuffix,
    DomainKeyword,
    IpCidr,
    Port,
    /// Country code or list from geoip.dat ("us", "de", "private" …).
    Geoip,
    /// Category from geosite.dat ("category-ads-all", "google" …).
    Geosite,
    /// Executable name of the application ("telegram.exe").
    ProcessName,
}

impl RuleKind {
    pub fn label(self) -> &'static str {
        match self {
            RuleKind::Domain => "domain",
            RuleKind::DomainSuffix => "domain_suffix",
            RuleKind::DomainKeyword => "domain_keyword",
            RuleKind::IpCidr => "ip_cidr",
            RuleKind::Port => "port",
            RuleKind::Geoip => "geoip",
            RuleKind::Geosite => "geosite",
            RuleKind::ProcessName => "process_name",
        }
    }

    /// Rules of this kind can only match when the destination IP is known.
    pub fn needs_ip(self) -> bool {
        matches!(self, RuleKind::IpCidr | RuleKind::Geoip)
    }
}

fn yes() -> bool {
    true
}

/// `target`: "proxy" (selected node) | "direct" | "block" | "node:<id>".
/// `value` may hold several comma-separated values.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub kind: RuleKind,
    pub value: String,
    pub target: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

// ---------------------------------------------------------------- settings / state

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Local proxy + Windows system proxy settings.
    #[default]
    SystemProxy,
    /// Local proxy only; apps are configured manually.
    Proxy,
    /// All traffic through the TUN interface.
    Tun,
}

/// Mode used when Mosaic connects automatically at start.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StartupMode {
    /// Whatever mode was used last.
    #[default]
    Last,
    SystemProxy,
    Proxy,
    Tun,
}

/// Android: which apps use the VPN.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppsMode {
    /// Every app.
    #[default]
    All,
    /// Every app except the chosen ones.
    Bypass,
    /// Only the chosen apps.
    Only,
}

/// How IP-based rules treat domain destinations.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DomainStrategy {
    /// Never resolve: IP rules only see connections made to an IP.
    #[default]
    AsIs,
    /// If no rule matched by domain, resolve and try the IP rules again.
    IpIfNonMatch,
}

/// Where names are resolved when IP rules need an address.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IpRuleDns {
    /// Through the active node to the remote DNS server (private).
    #[default]
    Remote,
    /// With the local/direct DNS (more accurate for GeoIP, but visible locally).
    Local,
}

/// Which implementation provides TUN mode.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TunStack {
    /// sing-box as the TUN adapter, routing still in the hub (stable).
    #[default]
    Singbox,
    /// Mosaic's own TUN stack (experimental, needs the `native-tun` build).
    Native,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    pub listen_port: u16,
    pub final_target: String,
    pub block_quic: bool,
    pub test_url: String,
    pub user_agent: String,
    /// Plain DNS used in TUN mode for node server names and direct traffic.
    pub direct_dns: String,
    pub domain_strategy: DomainStrategy,
    pub tun_stack: TunStack,
    /// "auto", "en", "ru" … — used only by the UI.
    pub ui_language: String,
    /// Windows: block DNS outside the TUN interface while TUN is up.
    pub dns_leak_protection: bool,
    /// DNS server queried through the tunnel ("1.1.1.1", "9.9.9.9:53" …).
    pub remote_dns: String,
    pub ip_rule_dns: IpRuleDns,
    /// Closing the window hides Mosaic to the tray.
    pub close_to_tray: bool,
    /// Start with Windows (minimized to the tray).
    pub autostart: bool,
    /// Connect automatically when Mosaic starts.
    pub auto_connect: bool,
    pub startup_mode: StartupMode,
    /// Listen on all interfaces so other devices in the LAN can use the proxy.
    pub allow_lan: bool,
    /// Smart Connect: keep the fastest working node selected automatically.
    pub smart_connect: bool,
    /// Refresh subscriptions every N hours (0 = only manually).
    pub sub_update_hours: u32,
    /// Check GitHub for new Mosaic versions once a day.
    pub check_updates: bool,
    /// Android: which apps go through the VPN, and the chosen package names.
    pub apps_mode: AppsMode,
    pub apps: Vec<String>,
    /// TUN: STUN (WebRTC address discovery) always goes through the active node, even when
    /// a rule sends the destination direct — otherwise a web page learns the real IP.
    pub webrtc_protection: bool,
    /// TUN: if the tunnel goes down without the user disconnecting (Mosaic crashed, a
    /// reconnect failed), block all traffic outside it instead of letting it leak.
    pub kill_switch: bool,
    /// Logs: one line per connection (opened / closed, rule, result) — for finding out why
    /// a site or a call does not work. Off by default: it is a lot of lines.
    pub verbose_log: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::SystemProxy,
            listen_port: 7890,
            final_target: "proxy".into(),
            block_quic: true,
            test_url: "http://www.gstatic.com/generate_204".into(),
            user_agent: "Mosaic/0.1 (v2rayN-compatible)".into(),
            direct_dns: "8.8.8.8".into(),
            domain_strategy: DomainStrategy::AsIs,
            tun_stack: TunStack::Singbox,
            ui_language: "auto".into(),
            dns_leak_protection: true,
            remote_dns: "1.1.1.1".into(),
            ip_rule_dns: IpRuleDns::Remote,
            close_to_tray: true,
            autostart: false,
            auto_connect: false,
            startup_mode: StartupMode::Last,
            allow_lan: false,
            smart_connect: false,
            sub_update_hours: 12,
            check_updates: true,
            apps_mode: AppsMode::All,
            apps: Vec::new(),
            webrtc_protection: true,
            kill_switch: false,
            verbose_log: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub url: String,
    #[serde(default)]
    pub name: String,
    /// From the provider's `subscription-userinfo` header (bytes; expire = unix seconds).
    #[serde(default)]
    pub upload: Option<u64>,
    #[serde(default)]
    pub download: Option<u64>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub expire: Option<u64>,
    #[serde(default)]
    pub updated_ms: u64,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub nodes: Vec<Node>,
    pub subscriptions: Vec<Subscription>,
    pub rules: Vec<Rule>,
    pub settings: Settings,
    pub selected: Option<String>,
    /// True while Mosaic owns the Windows system proxy; used to clean up after a crash.
    pub proxy_dirty: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            subscriptions: Vec::new(),
            rules: crate::rules::default_rules(),
            settings: Settings::default(),
            selected: None,
            proxy_dirty: false,
        }
    }
}
