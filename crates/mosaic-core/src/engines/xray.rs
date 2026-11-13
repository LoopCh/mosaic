//! IR -> Xray-core JSON.

use serde_json::{json, Value};

use super::{or_default, sni_or, Slot};
use crate::model::{Node, Protocol, Security, Transport};

pub fn supports(n: &Node) -> bool {
    match n.protocol {
        Protocol::Vless { .. } | Protocol::Vmess { .. } | Protocol::Trojan { .. } | Protocol::Shadowsocks { .. } => true,
        // Plain proxies: no transports; an HTTP proxy may sit behind TLS.
        Protocol::Socks { .. } => n.transport == Transport::Tcp && n.security == Security::None,
        Protocol::Http { .. } => n.transport == Transport::Tcp && !matches!(n.security, Security::Reality { .. }),
        _ => false,
    }
}

/// `{ "address", "port", "users": [{ "user", "pass" }] }` — the server entry of Xray's
/// socks / http outbounds; no login, no `users`.
fn proxy_server(n: &Node, username: &str, password: &str) -> Value {
    let mut srv = json!({ "address": n.server, "port": n.port });
    if !username.is_empty() {
        srv["users"] = json!([{ "user": username, "pass": password }]);
    }
    srv
}

pub fn compile(slots: &[Slot]) -> Result<Value, String> {
    let mut inbounds = Vec::new();
    let mut outbounds = Vec::new();
    let mut rules = Vec::new();
    for (i, (n, port)) in slots.iter().enumerate() {
        let it = format!("in-{i}");
        let ot = format!("out-{i}");
        inbounds.push(json!({
            "tag": it.clone(),
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": true, "ip": "127.0.0.1" }
        }));
        let mut ob = outbound(n)?;
        ob["tag"] = json!(ot.clone());
        outbounds.push(ob);
        rules.push(json!({ "inboundTag": [it], "outboundTag": ot }));
    }
    outbounds.push(json!({ "tag": "direct", "protocol": "freedom" }));
    Ok(json!({
        "log": { "loglevel": "warning", "access": "none" },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "routing": { "domainStrategy": "AsIs", "rules": rules }
    }))
}

fn outbound(n: &Node) -> Result<Value, String> {
    let stream = stream_settings(n)?;
    let ob = match &n.protocol {
        Protocol::Vless { uuid, flow } => {
            let mut user = json!({ "id": uuid, "encryption": "none" });
            if !flow.is_empty() {
                user["flow"] = json!(flow);
            }
            json!({
                "protocol": "vless",
                "settings": { "vnext": [{ "address": n.server, "port": n.port, "users": [user] }] },
                "streamSettings": stream
            })
        }
        Protocol::Vmess { uuid, cipher, .. } => {
            let sec = or_default(cipher, "auto");
            json!({
                "protocol": "vmess",
                "settings": { "vnext": [{ "address": n.server, "port": n.port, "users": [{ "id": uuid, "security": sec }] }] },
                "streamSettings": stream
            })
        }
        Protocol::Trojan { password } => json!({
            "protocol": "trojan",
            "settings": { "servers": [{ "address": n.server, "port": n.port, "password": password }] },
            "streamSettings": stream
        }),
        Protocol::Shadowsocks { method, password } => json!({
            "protocol": "shadowsocks",
            "settings": { "servers": [{ "address": n.server, "port": n.port, "method": method, "password": password }] },
            "streamSettings": stream
        }),
        Protocol::Socks { username, password } => json!({
            "protocol": "socks",
            "settings": { "servers": [proxy_server(n, username, password)] },
            "streamSettings": stream
        }),
        Protocol::Http { username, password } => json!({
            "protocol": "http",
            "settings": { "servers": [proxy_server(n, username, password)] },
            "streamSettings": stream
        }),
        other => return Err(format!("Xray does not support {}", other.name())),
    };
    Ok(ob)
}

fn stream_settings(n: &Node) -> Result<Value, String> {
    let mut s = json!({});
    match &n.transport {
        Transport::Tcp => {
            s["network"] = json!("tcp");
        }
        Transport::Ws { path, host } => {
            s["network"] = json!("ws");
            s["wsSettings"] = json!({ "path": path, "host": host });
        }
        Transport::Grpc { service_name } => {
            s["network"] = json!("grpc");
            s["grpcSettings"] = json!({ "serviceName": service_name });
        }
        Transport::Httpupgrade { path, host } => {
            s["network"] = json!("httpupgrade");
            s["httpupgradeSettings"] = json!({ "path": path, "host": host });
        }
        Transport::Xhttp { path, host, mode } => {
            let mode = or_default(mode, "auto");
            s["network"] = json!("xhttp");
            s["xhttpSettings"] = json!({ "path": path, "host": host, "mode": mode });
        }
    }
    match &n.security {
        Security::None => {
            s["security"] = json!("none");
        }
        Security::Tls { sni, alpn, fingerprint, insecure } => {
            let mut t = json!({ "serverName": sni_or(sni, &n.server) });
            if !alpn.is_empty() {
                t["alpn"] = json!(alpn);
            }
            if !fingerprint.is_empty() {
                t["fingerprint"] = json!(fingerprint);
            }
            if *insecure {
                t["allowInsecure"] = json!(true);
            }
            s["security"] = json!("tls");
            s["tlsSettings"] = t;
        }
        Security::Reality { sni, public_key, short_id, fingerprint, spider_x } => {
            let mut r = json!({
                "serverName": sni_or(sni, &n.server),
                "fingerprint": or_default(fingerprint, "chrome"),
                "publicKey": public_key,
                "shortId": short_id
            });
            if !spider_x.is_empty() {
                r["spiderX"] = json!(spider_x);
            }
            s["security"] = json!("reality");
            s["realitySettings"] = r;
        }
    }
    Ok(s)
}
