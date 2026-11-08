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
}

impl Protocol {
    pub fn name(&self) -> &'static str {
        match self {
            Protocol::Vless { .. } => "VLESS",
            Protocol::Vmess { .. } => "VMess",
            Protocol::Trojan { .. } => "Trojan",
            Protocol::Shadowsocks { .. } => "Shadowsocks",
            Protocol::Hysteria2 { .. } => "Hysteria2",
            Protocol::Tuic { .. } => "TUIC",
            Protocol::Wireguard { .. } => "WireGuard",
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
                Err(format!("{} не поддерживает {}", k.label(), self.summary()))
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
            .ok_or_else(|| format!("ни одно ядро не поддерживает {}", self.summary()))
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
}

impl RuleKind {
    pub fn label(self) -> &'static str {
        match self {
            RuleKind::Domain => "domain",
            RuleKind::DomainSuffix => "domain_suffix",
            RuleKind::DomainKeyword => "domain_keyword",
            RuleKind::IpCidr => "ip_cidr",
            RuleKind::Port => "port",
        }
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
    #[default]
    SystemProxy,
    Tun,
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
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub url: String,
    #[serde(default)]
    pub name: String,
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
