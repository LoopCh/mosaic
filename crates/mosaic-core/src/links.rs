//! Import: share links (vless, vmess, trojan, ss, hy2/hysteria2, tuic,
//! wireguard/wg), base64 subscriptions and WireGuard `.conf` files.

use crate::b64;
use crate::model::*;
use crate::util::{node_id, pct_decode};

#[derive(Debug, Default)]
pub struct ImportResult {
    pub nodes: Vec<Node>,
    pub errors: Vec<String>,
}

/// Parse any text the user may paste or a subscription may return.
pub fn parse_text(text: &str) -> ImportResult {
    let mut res = ImportResult::default();
    let t = text.trim().trim_start_matches('\u{feff}');
    if t.is_empty() {
        return res;
    }
    if t.contains("[Interface]") && t.contains("[Peer]") {
        match parse_wg_conf(t) {
            Ok(n) => res.nodes.push(n),
            Err(e) => res.errors.push(format!("WireGuard .conf: {e}")),
        }
        return res;
    }
    let decoded;
    let body: &str = if t.contains("://") {
        t
    } else {
        match b64::decode_str(t) {
            Some(d) if d.contains("://") => {
                decoded = d;
                &decoded
            }
            _ => {
                res.errors.push("не похоже ни на ссылки, ни на base64-подписку".into());
                return res;
            }
        }
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match parse_link(line) {
            Ok(n) => res.nodes.push(n),
            Err(e) => res.errors.push(format!("{}: {e}", shorten(line))),
        }
    }
    res
}

fn shorten(s: &str) -> String {
    let cut: String = s.chars().take(48).collect();
    if cut.len() < s.len() {
        format!("{cut}…")
    } else {
        cut
    }
}

pub fn parse_link(link: &str) -> Result<Node, String> {
    let scheme = link.split("://").next().unwrap_or("").to_ascii_lowercase();
    match scheme.as_str() {
        "vless" => parse_vless(link),
        "vmess" => parse_vmess(link),
        "trojan" => parse_trojan(link),
        "ss" => parse_ss(link),
        "hy2" | "hysteria2" => parse_hy2(link),
        "tuic" => parse_tuic(link),
        "wireguard" | "wg" => parse_wg_link(link),
        "" => Err("нет схемы".into()),
        other => Err(format!("схема {other}:// пока не поддерживается")),
    }
}

// ---------------------------------------------------------------- URL helper

struct Url {
    user: String,
    host: String,
    port: u16,
    query: Vec<(String, String)>,
    name: String,
}

impl Url {
    fn q(&self, key: &str) -> String {
        self.query
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    fn q_any(&self, keys: &[&str]) -> String {
        for k in keys {
            let v = self.q(k);
            if !v.is_empty() {
                return v;
            }
        }
        String::new()
    }

    fn flag(&self, keys: &[&str]) -> bool {
        matches!(self.q_any(keys).to_ascii_lowercase().as_str(), "1" | "true" | "yes")
    }

    fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            self.name.trim().to_string()
        }
    }
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (pct_decode(k), pct_decode(v)),
            None => (pct_decode(p), String::new()),
        })
        .collect()
}

pub(crate) fn split_host_port(s: &str) -> Result<(String, u16), String> {
    let (host, port_s) = if let Some(r) = s.strip_prefix('[') {
        let (h, p) = r.split_once(']').ok_or("плохой IPv6-адрес")?;
        (h.to_string(), p.trim_start_matches(':'))
    } else {
        let (h, p) = s.rsplit_once(':').ok_or("нет порта")?;
        (h.to_string(), p)
    };
    // Port ranges like "443,20000-30000" (Hysteria2 port hopping): take the first port.
    let digits: String = port_s.chars().take_while(|c| c.is_ascii_digit()).collect();
    let port: u16 = digits.parse().map_err(|_| format!("плохой порт «{port_s}»"))?;
    if host.is_empty() {
        return Err("пустой адрес сервера".into());
    }
    if port == 0 {
        return Err("порт 0".into());
    }
    Ok((host, port))
}

fn parse_url(link: &str) -> Result<Url, String> {
    let (_, rest) = link.split_once("://").ok_or("нет схемы")?;
    let (rest, name) = match rest.split_once('#') {
        Some((a, b)) => (a, pct_decode(b)),
        None => (rest, String::new()),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((a, b)) => (a, parse_query(b)),
        None => (rest, Vec::new()),
    };
    let rest = rest.trim_end_matches('/');
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (pct_decode(u), h),
        None => (String::new(), rest),
    };
    let (host, port) = split_host_port(hostport)?;
    Ok(Url { user, host, port, query, name })
}

fn or_slash(p: String) -> String {
    if p.is_empty() {
        "/".into()
    } else {
        p
    }
}

fn csv(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

fn transport_from(u: &Url) -> Result<Transport, String> {
    let t = u.q("type").to_ascii_lowercase();
    Ok(match t.as_str() {
        "" | "tcp" | "raw" => {
            if u.q("headerType").eq_ignore_ascii_case("http") {
                return Err("tcp с HTTP-обфускацией пока не поддерживается".into());
            }
            Transport::Tcp
        }
        "ws" | "websocket" => Transport::Ws { path: or_slash(u.q("path")), host: u.q("host") },
        "grpc" | "gun" => Transport::Grpc { service_name: u.q_any(&["serviceName", "service_name", "path"]) },
        "httpupgrade" => Transport::Httpupgrade { path: or_slash(u.q("path")), host: u.q("host") },
        "xhttp" | "splithttp" => Transport::Xhttp { path: or_slash(u.q("path")), host: u.q("host"), mode: u.q("mode") },
        other => return Err(format!("транспорт «{other}» пока не поддерживается")),
    })
}

fn security_from(u: &Url, default_tls: bool) -> Security {
    let s = u.q("security").to_ascii_lowercase();
    let sni = u.q_any(&["sni", "peer", "serverName"]);
    let fingerprint = u.q_any(&["fp", "fingerprint"]);
    match s.as_str() {
        "reality" => Security::Reality {
            sni,
            public_key: u.q_any(&["pbk", "publicKey", "public_key"]),
            short_id: u.q_any(&["sid", "shortId", "short_id"]),
            fingerprint,
            spider_x: u.q_any(&["spx", "spiderX"]),
        },
        "tls" | "xtls" => Security::Tls {
            sni,
            alpn: csv(&u.q("alpn")),
            fingerprint,
            insecure: u.flag(&["allowInsecure", "insecure", "allow_insecure"]),
        },
        "none" => Security::None,
        _ if default_tls => Security::Tls {
            sni,
            alpn: csv(&u.q("alpn")),
            fingerprint,
            insecure: u.flag(&["allowInsecure", "insecure", "allow_insecure"]),
        },
        _ => Security::None,
    }
}

fn node(link: &str, name: String, server: String, port: u16, protocol: Protocol, transport: Transport, security: Security) -> Node {
    Node {
        id: node_id(link),
        name,
        server,
        port,
        protocol,
        transport,
        security,
        engine: EngineChoice::Auto,
        source: None,
        link: link.to_string(),
    }
}

// ---------------------------------------------------------------- per-scheme parsers

fn parse_vless(link: &str) -> Result<Node, String> {
    let u = parse_url(link)?;
    if u.user.is_empty() {
        return Err("нет UUID".into());
    }
    let p = Protocol::Vless { uuid: u.user.clone(), flow: u.q("flow") };
    Ok(node(link, u.display_name(), u.host.clone(), u.port, p, transport_from(&u)?, security_from(&u, false)))
}

fn parse_trojan(link: &str) -> Result<Node, String> {
    let u = parse_url(link)?;
    if u.user.is_empty() {
        return Err("нет пароля".into());
    }
    let p = Protocol::Trojan { password: u.user.clone() };
    Ok(node(link, u.display_name(), u.host.clone(), u.port, p, transport_from(&u)?, security_from(&u, true)))
}

fn js(v: &serde_json::Value, k: &str) -> String {
    match v.get(k) {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn parse_vmess(link: &str) -> Result<Node, String> {
    let (_, payload) = link.split_once("://").ok_or("нет схемы")?;
    let payload = payload.split('#').next().unwrap_or(payload).trim();
    let json = b64::decode_str(payload).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let Some(v) = json else {
        // Some generators emit vmess in the same URL form as vless.
        let u = parse_url(link)?;
        let p = Protocol::Vmess { uuid: u.user.clone(), alter_id: 0, cipher: u.q_any(&["encryption", "security_cipher"]) };
        return Ok(node(link, u.display_name(), u.host.clone(), u.port, p, transport_from(&u)?, security_from(&u, false)));
    };
    let server = js(&v, "add");
    let port: u16 = js(&v, "port").parse().map_err(|_| "плохой порт в vmess")?;
    let uuid = js(&v, "id");
    if server.is_empty() || uuid.is_empty() {
        return Err("в vmess нет адреса или id".into());
    }
    let net = js(&v, "net").to_ascii_lowercase();
    let path = js(&v, "path");
    let host = js(&v, "host");
    let transport = match net.as_str() {
        "" | "tcp" | "raw" => {
            if js(&v, "type").eq_ignore_ascii_case("http") {
                return Err("tcp с HTTP-обфускацией пока не поддерживается".into());
            }
            Transport::Tcp
        }
        "ws" => Transport::Ws { path: or_slash(path), host },
        "grpc" => Transport::Grpc { service_name: path },
        "httpupgrade" => Transport::Httpupgrade { path: or_slash(path), host },
        "xhttp" | "splithttp" => Transport::Xhttp { path: or_slash(path), host, mode: js(&v, "type") },
        other => return Err(format!("транспорт «{other}» пока не поддерживается")),
    };
    let security = if js(&v, "tls").eq_ignore_ascii_case("tls") {
        let sni = js(&v, "sni");
        Security::Tls {
            sni: if sni.is_empty() { js(&v, "host") } else { sni },
            alpn: csv(&js(&v, "alpn")),
            fingerprint: js(&v, "fp"),
            insecure: matches!(js(&v, "allowInsecure").as_str(), "1" | "true"),
        }
    } else {
        Security::None
    };
    let name = js(&v, "ps");
    let name = if name.is_empty() { format!("{server}:{port}") } else { name };
    let p = Protocol::Vmess { uuid, alter_id: js(&v, "aid").parse().unwrap_or(0), cipher: js(&v, "scy") };
    Ok(node(link, name, server, port, p, transport, security))
}

fn parse_ss(link: &str) -> Result<Node, String> {
    let (_, rest) = link.split_once("://").ok_or("нет схемы")?;
    let (rest, name) = match rest.split_once('#') {
        Some((a, b)) => (a, pct_decode(b)),
        None => (rest, String::new()),
    };
    let query = rest.split_once('?').map(|(_, q)| parse_query(q)).unwrap_or_default();
    if query.iter().any(|(k, v)| k == "plugin" && !v.is_empty()) {
        return Err("Shadowsocks-плагины пока не поддерживаются".into());
    }
    let rest = rest.split('?').next().unwrap_or(rest).trim_end_matches('/');
    let (creds, hostport) = if let Some((userinfo, hp)) = rest.rsplit_once('@') {
        let ui = pct_decode(userinfo);
        let creds = if ui.contains(':') { ui } else { b64::decode_str(&ui).ok_or("не удалось декодировать userinfo")? };
        (creds, hp.to_string())
    } else {
        let dec = b64::decode_str(rest).ok_or("ss-ссылка не в base64")?;
        let (c, hp) = dec.rsplit_once('@').ok_or("нет адреса сервера")?;
        (c.to_string(), hp.to_string())
    };
    let (method, password) = creds.split_once(':').ok_or("нет метода или пароля")?;
    let (server, port) = split_host_port(&hostport)?;
    let name = if name.trim().is_empty() { format!("{server}:{port}") } else { name.trim().to_string() };
    let p = Protocol::Shadowsocks { method: method.to_string(), password: password.to_string() };
    Ok(node(link, name, server, port, p, Transport::Tcp, Security::None))
}

fn parse_hy2(link: &str) -> Result<Node, String> {
    let u = parse_url(link)?;
    let p = Protocol::Hysteria2 {
        password: u.user.clone(),
        obfs_password: u.q_any(&["obfs-password", "obfs_password"]),
        up_mbps: u.q("upmbps").parse().unwrap_or(0),
        down_mbps: u.q("downmbps").parse().unwrap_or(0),
    };
    let sec = Security::Tls {
        sni: u.q_any(&["sni", "peer"]),
        alpn: csv(&u.q("alpn")),
        fingerprint: String::new(),
        insecure: u.flag(&["insecure", "allowInsecure"]),
    };
    Ok(node(link, u.display_name(), u.host.clone(), u.port, p, Transport::Tcp, sec))
}

fn parse_tuic(link: &str) -> Result<Node, String> {
    let u = parse_url(link)?;
    let (uuid, password) = u.user.split_once(':').ok_or("в tuic-ссылке нужен uuid:password")?;
    let p = Protocol::Tuic {
        uuid: uuid.to_string(),
        password: password.to_string(),
        congestion: u.q_any(&["congestion_control", "congestion"]),
        udp_relay_mode: u.q("udp_relay_mode"),
    };
    let sec = Security::Tls {
        sni: u.q_any(&["sni", "peer"]),
        alpn: csv(&u.q("alpn")),
        fingerprint: String::new(),
        insecure: u.flag(&["allow_insecure", "insecure", "allowInsecure"]),
    };
    Ok(node(link, u.display_name(), u.host.clone(), u.port, p, Transport::Tcp, sec))
}

fn with_prefix(addr: &str) -> String {
    if addr.contains('/') {
        addr.to_string()
    } else if addr.contains(':') {
        format!("{addr}/128")
    } else {
        format!("{addr}/32")
    }
}

fn parse_wg_link(link: &str) -> Result<Node, String> {
    let u = parse_url(link)?;
    if u.user.is_empty() {
        return Err("нет приватного ключа".into());
    }
    let peer = u.q_any(&["publickey", "public_key", "peer_public_key", "pbk"]);
    if peer.is_empty() {
        return Err("нет публичного ключа пира".into());
    }
    let p = Protocol::Wireguard {
        private_key: u.user.clone(),
        peer_public_key: peer,
        pre_shared_key: u.q_any(&["presharedkey", "pre_shared_key", "psk"]),
        local_address: csv(&u.q_any(&["address", "ip", "local_address"])).iter().map(|a| with_prefix(a)).collect(),
        mtu: u.q("mtu").parse().unwrap_or(0),
        reserved: csv(&u.q("reserved")).iter().filter_map(|x| x.parse().ok()).collect(),
    };
    Ok(node(link, u.display_name(), u.host.clone(), u.port, p, Transport::Tcp, Security::None))
}

/// Standard WireGuard client config.
pub fn parse_wg_conf(text: &str) -> Result<Node, String> {
    let mut section = String::new();
    let (mut pk, mut addr, mut mtu, mut peer, mut psk, mut endpoint) =
        (String::new(), Vec::new(), 0u32, String::new(), String::new(), String::new());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_ascii_lowercase();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
        match (section.as_str(), k.as_str()) {
            ("interface", "privatekey") => pk = v,
            ("interface", "address") => addr = csv(&v).iter().map(|a| with_prefix(a)).collect(),
            ("interface", "mtu") => mtu = v.parse().unwrap_or(0),
            ("peer", "publickey") => peer = v,
            ("peer", "presharedkey") => psk = v,
            ("peer", "endpoint") => endpoint = v,
            _ => {}
        }
    }
    if pk.is_empty() || peer.is_empty() || endpoint.is_empty() {
        return Err("нужны PrivateKey, PublicKey и Endpoint".into());
    }
    let (server, port) = split_host_port(&endpoint)?;
    let seed = format!("wg:{pk}:{peer}:{endpoint}");
    let p = Protocol::Wireguard {
        private_key: pk,
        peer_public_key: peer,
        pre_shared_key: psk,
        local_address: addr,
        mtu,
        reserved: Vec::new(),
    };
    let mut n = node(&seed, format!("WireGuard {server}"), server, port, p, Transport::Tcp, Security::None);
    n.link = String::new();
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vless_reality() {
        let l = "vless://11111111-2222-3333-4444-555555555555@example.com:443?encryption=none&flow=xtls-rprx-vision&security=reality&sni=www.microsoft.com&fp=chrome&pbk=abcDEF_123-x&sid=6ba85179e30d4fc2&type=tcp#%D0%A4%D0%B8%D0%BD%D0%BB%D1%8F%D0%BD%D0%B4%D0%B8%D1%8F";
        let n = parse_link(l).unwrap();
        assert_eq!(n.name, "Финляндия");
        assert_eq!(n.server, "example.com");
        assert_eq!(n.port, 443);
        assert!(matches!(n.protocol, Protocol::Vless { ref flow, .. } if flow == "xtls-rprx-vision"));
        assert!(matches!(n.security, Security::Reality { ref public_key, ref short_id, .. } if public_key == "abcDEF_123-x" && short_id == "6ba85179e30d4fc2"));
        assert_eq!(n.effective_engine().unwrap(), EngineKind::Xray);
    }

    #[test]
    fn vless_xhttp_is_xray_only() {
        let n = parse_link("vless://id@1.2.3.4:443?type=xhttp&path=%2Fx&security=tls&sni=a.com#x").unwrap();
        assert_eq!(n.supported_engines(), vec![EngineKind::Xray]);
    }

    #[test]
    fn vless_ws_ipv6() {
        let n = parse_link("vless://id@[2001:db8::1]:8443?type=ws&path=%2Fws%3Fed%3D2048&host=cdn.example.com&security=tls#v6").unwrap();
        assert_eq!(n.server, "2001:db8::1");
        assert_eq!(n.port, 8443);
        assert_eq!(n.transport, Transport::Ws { path: "/ws?ed=2048".into(), host: "cdn.example.com".into() });
    }

    #[test]
    fn vmess_json() {
        let json = r#"{"v":"2","ps":"vm","add":"vm.example.com","port":"443","id":"uuid-1","aid":"0","scy":"auto","net":"ws","type":"none","host":"h.example.com","path":"/p","tls":"tls","sni":""}"#;
        let link = format!("vmess://{}", encode(json));
        let n = parse_link(&link).unwrap();
        assert_eq!(n.server, "vm.example.com");
        assert_eq!(n.port, 443);
        assert!(matches!(n.security, Security::Tls { ref sni, .. } if sni == "h.example.com"));
    }

    #[test]
    fn ss_forms() {
        // SIP002 with base64 userinfo
        let a = parse_link("ss://YWVzLTI1Ni1nY206cGFzcw@1.2.3.4:8388#A").unwrap();
        assert!(matches!(a.protocol, Protocol::Shadowsocks { ref method, ref password } if method == "aes-256-gcm" && password == "pass"));
        // legacy fully-encoded
        let b = parse_link(&format!("ss://{}#B", encode("chacha20-ietf-poly1305:p@ss@5.6.7.8:443"))).unwrap();
        assert_eq!(b.server, "5.6.7.8");
        assert!(matches!(b.protocol, Protocol::Shadowsocks { ref password, .. } if password == "p@ss"));
        // plain 2022
        let c = parse_link("ss://2022-blake3-aes-128-gcm:a2V5%2Bkey%3D@9.9.9.9:443#C").unwrap();
        assert!(matches!(c.protocol, Protocol::Shadowsocks { ref password, .. } if password == "a2V5+key="));
        assert_eq!(c.effective_engine().unwrap(), EngineKind::Singbox);
    }

    #[test]
    fn hy2_tuic_wg() {
        let h = parse_link("hy2://secret@h.example.com:443,20000-30000/?sni=h.example.com&obfs=salamander&obfs-password=xyz&insecure=1#H").unwrap();
        assert_eq!(h.port, 443);
        assert_eq!(h.supported_engines(), vec![EngineKind::Singbox]);
        let t = parse_link("tuic://uuid:pw@t.example.com:443?congestion_control=bbr&alpn=h3&sni=t.example.com#T").unwrap();
        assert!(matches!(t.protocol, Protocol::Tuic { ref password, .. } if password == "pw"));
        let w = parse_link("wireguard://cHJpdmF0ZQ%3D%3D@1.1.1.1:51820?publickey=cHVibGlj&address=10.0.0.2,fd00::2&mtu=1280&reserved=1,2,3#W").unwrap();
        assert!(matches!(w.protocol, Protocol::Wireguard { ref private_key, ref local_address, ref reserved, .. }
            if private_key == "cHJpdmF0ZQ==" && local_address == &vec!["10.0.0.2/32".to_string(), "fd00::2/128".to_string()] && reserved == &vec![1u8, 2, 3]));
    }

    #[test]
    fn wg_conf_and_subscription() {
        let conf = "[Interface]\nPrivateKey = aaa=\nAddress = 10.8.0.2/24\n\n[Peer]\nPublicKey = bbb=\nEndpoint = vpn.example.com:51820\nAllowedIPs = 0.0.0.0/0\n";
        let r = parse_text(conf);
        assert_eq!(r.nodes.len(), 1);
        assert_eq!(r.nodes[0].port, 51820);

        let sub = encode("vless://a@1.1.1.1:443?security=tls#one\ntrojan://pw@2.2.2.2:443#two\nfoo://bar\n");
        let r = parse_text(&sub);
        assert_eq!(r.nodes.len(), 2);
        assert_eq!(r.errors.len(), 1);
        assert!(matches!(r.nodes[1].security, Security::Tls { .. }));
    }

    // tiny encoder for tests only
    fn encode(s: &str) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let b = s.as_bytes();
        let mut out = String::new();
        for ch in b.chunks(3) {
            let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= ch.len() {
                    out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}
