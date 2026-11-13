//! Import of whole configs: Clash / mihomo YAML (`proxies:`) and sing-box JSON
//! (`outbounds` / `endpoints`). Both are read into `serde_json::Value` and mapped
//! to Mosaic's node model. Group and utility entries (selector, urltest, direct,
//! dns, block…) are skipped silently; unsupported proxy types are reported.
//!
//! Also: the `subscription-userinfo` header and subscription titles.

use serde_json::Value;

use crate::model::*;
use crate::util::node_id;

// ---------------------------------------------------------------- small accessors

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn b(v: &Value, k: &str) -> bool {
    match v.get(k) {
        Some(Value::Bool(x)) => *x,
        Some(Value::String(x)) => x == "true" || x == "1",
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}

fn u(v: &Value, k: &str) -> Option<u64> {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(x)) => x.trim().parse().ok(),
        _ => None,
    }
}

fn list(v: &Value, k: &str) -> Vec<String> {
    match v.get(k) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::String(x)) if !x.is_empty() => x.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect(),
        _ => Vec::new(),
    }
}

fn obj<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| x.is_object())
}

fn port_of(v: &Value, k: &str) -> Option<u16> {
    u(v, k).and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0)
}

fn make(name: String, server: String, port: u16, protocol: Protocol, transport: Transport, security: Security) -> Node {
    // No share link for config-imported nodes: the id comes from the content.
    let seed = serde_json::to_string(&(&name, &server, port, &protocol, &transport, &security)).unwrap_or_default();
    Node {
        id: node_id(&seed),
        name,
        server,
        port,
        protocol,
        transport,
        security,
        engine: EngineChoice::Auto,
        source: None,
        link: String::new(),
    }
}

/// Collect results: nodes, and errors for entries that looked like proxies.
type Mapped = (Vec<Node>, Vec<String>);

// ---------------------------------------------------------------- detection

/// Is this text a Clash/mihomo YAML config with proxies?
pub fn looks_like_clash(t: &str) -> bool {
    t.lines().any(|l| l.trim_end() == "proxies:" || l.starts_with("proxies:"))
}

/// Is this text a sing-box JSON config?
pub fn looks_like_singbox(t: &str) -> bool {
    let t = t.trim_start();
    t.starts_with('{') && (t.contains("\"outbounds\"") || t.contains("\"endpoints\""))
}

// ---------------------------------------------------------------- Clash / mihomo

pub fn parse_clash(text: &str) -> Result<Mapped, String> {
    let v: Value = serde_yaml::from_str(text).map_err(|e| format!("YAML: {e}"))?;
    let proxies = v.get("proxies").and_then(|p| p.as_array()).ok_or("no `proxies:` list")?;
    let mut out = (Vec::new(), Vec::new());
    for p in proxies {
        let name = s(p, "name");
        match clash_proxy(p) {
            Ok(Some(n)) => out.0.push(n),
            Ok(None) => {}
            Err(e) => out.1.push(format!("{}: {e}", if name.is_empty() { "?".into() } else { name })),
        }
    }
    Ok(out)
}

fn clash_transport(p: &Value) -> Result<Transport, String> {
    let net = s(p, "network");
    Ok(match net.as_str() {
        "" | "tcp" => Transport::Tcp,
        "ws" => {
            let o = obj(p, "ws-opts").cloned().unwrap_or(Value::Null);
            let host = obj(&o, "headers").map(|h| s(h, "Host")).unwrap_or_default();
            let path = s(&o, "path");
            Transport::Ws { path: if path.is_empty() { "/".into() } else { path }, host }
        }
        "grpc" => {
            let o = obj(p, "grpc-opts").cloned().unwrap_or(Value::Null);
            Transport::Grpc { service_name: s(&o, "grpc-service-name") }
        }
        "httpupgrade" => {
            let o = obj(p, "http-upgrade-opts").or_else(|| obj(p, "ws-opts")).cloned().unwrap_or(Value::Null);
            let host = obj(&o, "headers").map(|h| s(h, "Host")).unwrap_or_else(|| s(&o, "host"));
            let path = s(&o, "path");
            Transport::Httpupgrade { path: if path.is_empty() { "/".into() } else { path }, host }
        }
        "xhttp" | "splithttp" => {
            let o = obj(p, "xhttp-opts").cloned().unwrap_or(Value::Null);
            let path = s(&o, "path");
            Transport::Xhttp { path: if path.is_empty() { "/".into() } else { path }, host: s(&o, "host"), mode: s(&o, "mode") }
        }
        other => return Err(format!("transport `{other}` is not supported")),
    })
}

fn clash_security(p: &Value, default_tls: bool) -> Security {
    let fp = s(p, "client-fingerprint");
    let sni = {
        let x = s(p, "servername");
        if x.is_empty() { s(p, "sni") } else { x }
    };
    if let Some(r) = obj(p, "reality-opts") {
        return Security::Reality { sni, public_key: s(r, "public-key"), short_id: s(r, "short-id"), fingerprint: fp, spider_x: String::new() };
    }
    if b(p, "tls") || default_tls {
        return Security::Tls { sni, alpn: list(p, "alpn"), fingerprint: fp, insecure: b(p, "skip-cert-verify") };
    }
    Security::None
}

fn clash_proxy(p: &Value) -> Result<Option<Node>, String> {
    let ty = s(p, "type").to_ascii_lowercase();
    let name = s(p, "name");
    let server = s(p, "server");
    let port = port_of(p, "port");
    let need = |what: &str| format!("missing `{what}`");
    let (server, port) = match ty.as_str() {
        "ss" | "shadowsocks" | "vmess" | "vless" | "trojan" | "hysteria2" | "hy2" | "tuic" | "wireguard" | "socks5" | "http" => {
            if server.is_empty() {
                return Err(need("server"));
            }
            (server, port.ok_or_else(|| need("port"))?)
        }
        // groups and anything else that is not a proxy we can run
        "direct" | "reject" | "dns" | "select" | "url-test" | "fallback" | "load-balance" | "relay" => return Ok(None),
        other => return Err(format!("type `{other}` is not supported")),
    };
    let node = match ty.as_str() {
        "ss" | "shadowsocks" => {
            if !s(p, "plugin").is_empty() {
                return Err("Shadowsocks plugins are not supported".into());
            }
            make(name, server, port, Protocol::Shadowsocks { method: s(p, "cipher"), password: s(p, "password") }, Transport::Tcp, Security::None)
        }
        "vmess" => {
            let proto = Protocol::Vmess {
                uuid: s(p, "uuid"),
                alter_id: u(p, "alterId").unwrap_or(0) as u32,
                cipher: { let c = s(p, "cipher"); if c.is_empty() { "auto".into() } else { c } },
            };
            make(name, server, port, proto, clash_transport(p)?, clash_security(p, false))
        }
        "vless" => {
            let proto = Protocol::Vless { uuid: s(p, "uuid"), flow: s(p, "flow") };
            make(name, server, port, proto, clash_transport(p)?, clash_security(p, false))
        }
        "trojan" => make(name, server, port, Protocol::Trojan { password: s(p, "password") }, clash_transport(p)?, clash_security(p, true)),
        "hysteria2" | "hy2" => {
            let proto = Protocol::Hysteria2 {
                password: { let x = s(p, "password"); if x.is_empty() { s(p, "auth") } else { x } },
                obfs_password: s(p, "obfs-password"),
                up_mbps: mbps(&s(p, "up")),
                down_mbps: mbps(&s(p, "down")),
            };
            make(name, server, port, proto, Transport::Tcp, clash_security(p, true))
        }
        "tuic" => {
            let proto = Protocol::Tuic {
                uuid: s(p, "uuid"),
                password: s(p, "password"),
                congestion: s(p, "congestion-controller"),
                udp_relay_mode: s(p, "udp-relay-mode"),
            };
            make(name, server, port, proto, Transport::Tcp, clash_security(p, true))
        }
        "socks5" => make(name, server, port, Protocol::Socks { username: s(p, "username"), password: s(p, "password") }, Transport::Tcp, Security::None),
        "http" => make(
            name,
            server,
            port,
            Protocol::Http { username: s(p, "username"), password: s(p, "password") },
            Transport::Tcp,
            clash_security(p, false),
        ),
        "wireguard" => {
            let mut addrs = Vec::new();
            for k in ["ip", "ipv6"] {
                let a = s(p, k);
                if !a.is_empty() {
                    addrs.push(if a.contains('/') { a } else if a.contains(':') { format!("{a}/128") } else { format!("{a}/32") });
                }
            }
            let proto = Protocol::Wireguard {
                private_key: s(p, "private-key"),
                peer_public_key: s(p, "public-key"),
                pre_shared_key: { let x = s(p, "pre-shared-key"); if x.is_empty() { s(p, "preshared-key") } else { x } },
                local_address: addrs,
                mtu: u(p, "mtu").unwrap_or(0) as u32,
                reserved: reserved(p.get("reserved")),
            };
            make(name, server, port, proto, Transport::Tcp, Security::None)
        }
        _ => unreachable!(),
    };
    Ok(Some(node))
}

/// "100 Mbps", "100", 100 → 100
fn mbps(x: &str) -> u32 {
    x.trim().split_whitespace().next().and_then(|n| n.parse::<f64>().ok()).map(|n| n as u32).unwrap_or(0)
}

fn reserved(v: Option<&Value>) -> Vec<u8> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_u64()).filter_map(|x| u8::try_from(x).ok()).collect(),
        Some(Value::String(x)) => crate::b64::decode(x).unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------- sing-box

pub fn parse_singbox(text: &str) -> Result<Mapped, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON: {e}"))?;
    let mut out = (Vec::new(), Vec::new());
    let items = ["outbounds", "endpoints"].iter().filter_map(|k| v.get(*k).and_then(|x| x.as_array())).flatten();
    for o in items {
        let tag = s(o, "tag");
        match singbox_outbound(o) {
            Ok(Some(n)) => out.0.push(n),
            Ok(None) => {}
            Err(e) => out.1.push(format!("{}: {e}", if tag.is_empty() { "?".into() } else { tag })),
        }
    }
    Ok(out)
}

fn sb_transport(o: &Value) -> Result<Transport, String> {
    let Some(t) = obj(o, "transport") else { return Ok(Transport::Tcp) };
    let host_of = |t: &Value| obj(t, "headers").map(|h| s(h, "Host")).filter(|h| !h.is_empty()).unwrap_or_else(|| s(t, "host"));
    let path = |t: &Value| {
        let p = s(t, "path");
        if p.is_empty() { "/".to_string() } else { p }
    };
    Ok(match s(t, "type").as_str() {
        "ws" => Transport::Ws { path: path(t), host: host_of(t) },
        "grpc" => Transport::Grpc { service_name: s(t, "service_name") },
        "httpupgrade" => Transport::Httpupgrade { path: path(t), host: host_of(t) },
        other => return Err(format!("transport `{other}` is not supported")),
    })
}

fn sb_security(o: &Value) -> Security {
    let Some(tls) = obj(o, "tls").filter(|t| b(t, "enabled")) else { return Security::None };
    let fp = obj(tls, "utls").filter(|x| b(x, "enabled")).map(|x| s(x, "fingerprint")).unwrap_or_default();
    let sni = s(tls, "server_name");
    if let Some(r) = obj(tls, "reality").filter(|r| b(r, "enabled")) {
        return Security::Reality { sni, public_key: s(r, "public_key"), short_id: s(r, "short_id"), fingerprint: fp, spider_x: String::new() };
    }
    Security::Tls { sni, alpn: list(tls, "alpn"), fingerprint: fp, insecure: b(tls, "insecure") }
}

fn singbox_outbound(o: &Value) -> Result<Option<Node>, String> {
    let ty = s(o, "type");
    let name = s(o, "tag");
    let need = |what: &str| format!("missing `{what}`");
    let node = match ty.as_str() {
        "direct" | "block" | "dns" | "selector" | "urltest" => return Ok(None),
        "wireguard" => {
            // 1.11+ endpoint (peers[]) or legacy outbound (server / peer_public_key).
            let peer = o.get("peers").and_then(|p| p.as_array()).and_then(|p| p.first()).cloned();
            let (server, port, pubkey, psk, reserved_v) = match &peer {
                Some(p) => (s(p, "address"), port_of(p, "port"), s(p, "public_key"), s(p, "pre_shared_key"), p.get("reserved").cloned()),
                None => (s(o, "server"), port_of(o, "server_port"), s(o, "peer_public_key"), s(o, "pre_shared_key"), o.get("reserved").cloned()),
            };
            if server.is_empty() {
                return Err(need("server"));
            }
            let mut addrs = list(o, "address");
            if addrs.is_empty() {
                addrs = list(o, "local_address");
            }
            let proto = Protocol::Wireguard {
                private_key: s(o, "private_key"),
                peer_public_key: pubkey,
                pre_shared_key: psk,
                local_address: addrs,
                mtu: u(o, "mtu").unwrap_or(0) as u32,
                reserved: reserved(reserved_v.as_ref()),
            };
            make(name, server, port.ok_or_else(|| need("port"))?, proto, Transport::Tcp, Security::None)
        }
        "vless" | "vmess" | "trojan" | "shadowsocks" | "hysteria2" | "tuic" => {
            let server = s(o, "server");
            if server.is_empty() {
                return Err(need("server"));
            }
            let port = port_of(o, "server_port").ok_or_else(|| need("server_port"))?;
            let proto = match ty.as_str() {
                "vless" => Protocol::Vless { uuid: s(o, "uuid"), flow: s(o, "flow") },
                "vmess" => Protocol::Vmess {
                    uuid: s(o, "uuid"),
                    alter_id: u(o, "alter_id").unwrap_or(0) as u32,
                    cipher: { let c = s(o, "security"); if c.is_empty() { "auto".into() } else { c } },
                },
                "trojan" => Protocol::Trojan { password: s(o, "password") },
                "shadowsocks" => {
                    if !s(o, "plugin").is_empty() {
                        return Err("Shadowsocks plugins are not supported".into());
                    }
                    Protocol::Shadowsocks { method: s(o, "method"), password: s(o, "password") }
                }
                "hysteria2" => Protocol::Hysteria2 {
                    password: s(o, "password"),
                    obfs_password: obj(o, "obfs").map(|x| s(x, "password")).unwrap_or_default(),
                    up_mbps: u(o, "up_mbps").unwrap_or(0) as u32,
                    down_mbps: u(o, "down_mbps").unwrap_or(0) as u32,
                },
                _ => Protocol::Tuic {
                    uuid: s(o, "uuid"),
                    password: s(o, "password"),
                    congestion: s(o, "congestion_control"),
                    udp_relay_mode: s(o, "udp_relay_mode"),
                },
            };
            make(name, server, port, proto, sb_transport(o)?, sb_security(o))
        }
        "socks" | "http" => {
            let server = s(o, "server");
            if server.is_empty() {
                return Err(need("server"));
            }
            let port = port_of(o, "server_port").ok_or_else(|| need("server_port"))?;
            let (username, password) = (s(o, "username"), s(o, "password"));
            if ty == "socks" {
                let v = s(o, "version");
                if !(v.is_empty() || v == "5") {
                    return Err(format!("SOCKS{v} is not supported (only SOCKS5)"));
                }
                make(name, server, port, Protocol::Socks { username, password }, Transport::Tcp, Security::None)
            } else {
                make(name, server, port, Protocol::Http { username, password }, Transport::Tcp, sb_security(o))
            }
        }
        other => return Err(format!("type `{other}` is not supported")),
    };
    Ok(Some(node))
}

// ---------------------------------------------------------------- subscription headers

/// Traffic and expiry from `subscription-userinfo: upload=…; download=…; total=…; expire=…`.
#[derive(Debug, Default, PartialEq)]
pub struct UserInfo {
    pub upload: Option<u64>,
    pub download: Option<u64>,
    pub total: Option<u64>,
    /// Unix time, seconds.
    pub expire: Option<u64>,
}

pub fn parse_userinfo(h: &str) -> UserInfo {
    let mut ui = UserInfo::default();
    for part in h.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        // Some providers send floats ("1.5e10") — accept those too.
        let n = v.trim().parse::<u64>().ok().or_else(|| v.trim().parse::<f64>().ok().filter(|f| *f >= 0.0).map(|f| f as u64));
        match k.trim().to_ascii_lowercase().as_str() {
            "upload" => ui.upload = n,
            "download" => ui.download = n,
            "total" => ui.total = n,
            "expire" => ui.expire = n.filter(|x| *x > 0),
            _ => {}
        }
    }
    ui
}

/// Subscription title: `profile-title` ("base64:…" or plain), else the
/// Content-Disposition file name without extension.
pub fn subscription_title(profile_title: Option<&str>, content_disposition: Option<&str>) -> Option<String> {
    if let Some(t) = profile_title.map(str::trim).filter(|t| !t.is_empty()) {
        if let Some(b) = t.strip_prefix("base64:") {
            if let Some(d) = crate::b64::decode_str(b.trim()) {
                return Some(d.trim().to_string()).filter(|x| !x.is_empty());
            }
        }
        return Some(t.to_string());
    }
    let cd = content_disposition?;
    let name = if let Some(i) = cd.find("filename*=") {
        let v = cd[i + 10..].split(';').next()?.trim().trim_matches('"');
        let v = v.split_once("''").map(|(_, x)| x).unwrap_or(v);
        crate::util::pct_decode(v)
    } else {
        let i = cd.find("filename=")?;
        cd[i + 9..].split(';').next()?.trim().trim_matches('"').to_string()
    };
    let stem = name.rsplit_once('.').filter(|(_, ext)| ext.len() <= 5).map(|(s, _)| s.to_string()).unwrap_or(name);
    Some(stem).filter(|x| !x.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo() {
        let ui = parse_userinfo("upload=1024; download=2048; total=10737418240; expire=1767225600");
        assert_eq!(ui, UserInfo { upload: Some(1024), download: Some(2048), total: Some(10737418240), expire: Some(1767225600) });
        assert_eq!(parse_userinfo("upload=0;download=5;total=0;expire=0").expire, None);
        assert_eq!(parse_userinfo("total=1.5e9").total, Some(1_500_000_000));
    }

    #[test]
    fn titles() {
        assert_eq!(subscription_title(Some("base64:0JzQvtC5INCS0J/QnQ=="), None).as_deref(), Some("Мой ВПН"));
        assert_eq!(subscription_title(Some("Plain"), None).as_deref(), Some("Plain"));
        assert_eq!(subscription_title(None, Some("attachment; filename=\"Provider.yaml\"")).as_deref(), Some("Provider"));
        assert_eq!(subscription_title(None, Some("attachment; filename*=UTF-8''%D0%92%D0%9F%D0%9D.txt")).as_deref(), Some("ВПН"));
        assert_eq!(subscription_title(None, None), None);
    }

    const CLASH: &str = r#"
port: 7890
proxies:
  - {name: "SS", type: ss, server: 1.2.3.4, port: 8388, cipher: aes-256-gcm, password: pw}
  - name: Reality
    type: vless
    server: r.example.com
    port: 443
    uuid: 11111111-2222-3333-4444-555555555555
    flow: xtls-rprx-vision
    network: tcp
    tls: true
    servername: www.microsoft.com
    client-fingerprint: chrome
    reality-opts:
      public-key: PUBKEY
      short-id: ab12
  - name: WS
    type: vmess
    server: w.example.com
    port: 443
    uuid: 11111111-2222-3333-4444-555555555555
    alterId: 0
    cipher: auto
    tls: true
    network: ws
    ws-opts:
      path: /ray
      headers:
        Host: cdn.example.com
  - {name: HY2, type: hysteria2, server: h.example.com, port: 443, password: pw, sni: h.example.com, up: "50 Mbps"}
  - {name: Auto, type: url-test, proxies: [SS]}
  - {name: Old, type: ssr, server: x, port: 1}
proxy-groups:
  - {name: PROXY, type: select, proxies: [SS]}
"#;

    #[test]
    fn clash() {
        assert!(looks_like_clash(CLASH));
        let (nodes, errors) = parse_clash(CLASH).unwrap();
        assert_eq!(nodes.len(), 4, "{errors:?}");
        assert_eq!(errors.len(), 1);
        assert!(matches!(&nodes[0].protocol, Protocol::Shadowsocks { method, .. } if method == "aes-256-gcm"));
        assert!(matches!(&nodes[1].security, Security::Reality { public_key, short_id, sni, .. } if public_key == "PUBKEY" && short_id == "ab12" && sni == "www.microsoft.com"));
        assert!(matches!(&nodes[1].protocol, Protocol::Vless { flow, .. } if flow == "xtls-rprx-vision"));
        assert!(matches!(&nodes[2].transport, Transport::Ws { path, host } if path == "/ray" && host == "cdn.example.com"));
        assert!(matches!(&nodes[3].protocol, Protocol::Hysteria2 { up_mbps: 50, .. }));
        assert_ne!(nodes[0].id, nodes[1].id);
    }

    const SINGBOX: &str = r#"{
      "outbounds": [
        {"type": "selector", "tag": "proxy", "outbounds": ["a"]},
        {"type": "vless", "tag": "a", "server": "a.example.com", "server_port": 443, "uuid": "u", "flow": "xtls-rprx-vision",
         "tls": {"enabled": true, "server_name": "sni.example.com", "utls": {"enabled": true, "fingerprint": "chrome"},
                 "reality": {"enabled": true, "public_key": "PK", "short_id": "01"}}},
        {"type": "trojan", "tag": "t", "server": "t.example.com", "server_port": 443, "password": "pw",
         "tls": {"enabled": true, "server_name": "t.example.com"}, "transport": {"type": "grpc", "service_name": "svc"}},
        {"type": "tuic", "tag": "q", "server": "q.example.com", "server_port": 443, "uuid": "u", "password": "p",
         "congestion_control": "bbr", "tls": {"enabled": true, "alpn": ["h3"]}},
        {"type": "direct", "tag": "direct"}
      ],
      "endpoints": [
        {"type": "wireguard", "tag": "wg", "address": ["10.0.0.2/32"], "private_key": "PRIV",
         "peers": [{"address": "wg.example.com", "port": 51820, "public_key": "PUB", "reserved": [1, 2, 3]}]}
      ]
    }"#;

    #[test]
    fn singbox() {
        assert!(looks_like_singbox(SINGBOX));
        let (nodes, errors) = parse_singbox(SINGBOX).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(nodes.len(), 4);
        assert!(matches!(&nodes[0].security, Security::Reality { public_key, fingerprint, .. } if public_key == "PK" && fingerprint == "chrome"));
        assert!(matches!(&nodes[1].transport, Transport::Grpc { service_name } if service_name == "svc"));
        assert!(matches!(&nodes[2].security, Security::Tls { alpn, .. } if alpn == &vec!["h3".to_string()]));
        assert!(matches!(&nodes[3].protocol, Protocol::Wireguard { reserved, local_address, .. } if reserved == &vec![1, 2, 3] && local_address == &vec!["10.0.0.2/32".to_string()]));
        assert_eq!(nodes[3].port, 51820);
    }
}
