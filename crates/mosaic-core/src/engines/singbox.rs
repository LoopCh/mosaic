//! IR -> sing-box JSON. Targets sing-box 1.12+ (new DNS server format,
//! route actions, WireGuard endpoints).

use serde_json::{json, Value};

use super::{or_default, sni_or, Slot};
use crate::model::{Node, Protocol, Security, Transport};

pub fn supports(n: &Node) -> bool {
    !matches!(n.transport, Transport::Xhttp { .. })
}

pub fn compile(slots: &[Slot]) -> Result<Value, String> {
    let mut inbounds = Vec::new();
    let mut outbounds = Vec::new();
    let mut endpoints = Vec::new();
    let mut rules = Vec::new();
    for (i, (n, port)) in slots.iter().enumerate() {
        let it = format!("in-{i}");
        let ot = format!("out-{i}");
        inbounds.push(json!({ "type": "socks", "tag": it.clone(), "listen": "127.0.0.1", "listen_port": port }));
        let mut ob = outbound(n)?;
        ob["tag"] = json!(ot.clone());
        if matches!(n.protocol, Protocol::Wireguard { .. }) {
            endpoints.push(ob);
        } else {
            outbounds.push(ob);
        }
        rules.push(json!({ "inbound": [it], "outbound": ot }));
    }
    outbounds.push(json!({ "type": "direct", "tag": "direct" }));
    let mut cfg = json!({
        "log": { "level": "warn", "timestamp": false },
        "dns": { "servers": [{ "type": "local", "tag": "local" }] },
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": { "rules": rules, "final": "direct", "default_domain_resolver": "local" }
    });
    if !endpoints.is_empty() {
        cfg["endpoints"] = json!(endpoints);
    }
    Ok(cfg)
}

fn tls(n: &Node, force: bool, default_alpn: &[&str]) -> Option<Value> {
    match &n.security {
        Security::None if !force => None,
        Security::None => Some(json!({ "enabled": true, "server_name": n.server, "alpn": default_alpn })),
        Security::Tls { sni, alpn, fingerprint, insecure } => {
            let mut t = json!({ "enabled": true, "server_name": sni_or(sni, &n.server), "insecure": insecure });
            if !alpn.is_empty() {
                t["alpn"] = json!(alpn);
            } else if !default_alpn.is_empty() {
                t["alpn"] = json!(default_alpn);
            }
            if !fingerprint.is_empty() {
                t["utls"] = json!({ "enabled": true, "fingerprint": fingerprint });
            }
            Some(t)
        }
        Security::Reality { sni, public_key, short_id, fingerprint, .. } => Some(json!({
            "enabled": true,
            "server_name": sni_or(sni, &n.server),
            "utls": { "enabled": true, "fingerprint": or_default(fingerprint, "chrome") },
            "reality": { "enabled": true, "public_key": public_key, "short_id": short_id }
        })),
    }
}

fn transport(n: &Node) -> Result<Option<Value>, String> {
    Ok(match &n.transport {
        Transport::Tcp => None,
        Transport::Ws { path, host } => {
            let mut t = json!({ "type": "ws", "path": path });
            if !host.is_empty() {
                t["headers"] = json!({ "Host": host });
            }
            Some(t)
        }
        Transport::Grpc { service_name } => Some(json!({ "type": "grpc", "service_name": service_name })),
        Transport::Httpupgrade { path, host } => Some(json!({ "type": "httpupgrade", "path": path, "host": host })),
        Transport::Xhttp { .. } => return Err("sing-box does not support XHTTP — use Xray".into()),
    })
}

fn outbound(n: &Node) -> Result<Value, String> {
    let mut ob = match &n.protocol {
        Protocol::Vless { uuid, flow } => {
            let mut o = json!({ "type": "vless", "server": n.server, "server_port": n.port, "uuid": uuid, "packet_encoding": "xudp" });
            if !flow.is_empty() {
                o["flow"] = json!(flow);
            }
            o
        }
        Protocol::Vmess { uuid, alter_id, cipher } => json!({
            "type": "vmess", "server": n.server, "server_port": n.port, "uuid": uuid,
            "security": or_default(cipher, "auto"), "alter_id": alter_id
        }),
        Protocol::Trojan { password } => json!({ "type": "trojan", "server": n.server, "server_port": n.port, "password": password }),
        Protocol::Shadowsocks { method, password } => json!({
            "type": "shadowsocks", "server": n.server, "server_port": n.port, "method": method, "password": password
        }),
        Protocol::Hysteria2 { password, obfs_password, up_mbps, down_mbps } => {
            let mut o = json!({ "type": "hysteria2", "server": n.server, "server_port": n.port, "password": password });
            if !obfs_password.is_empty() {
                o["obfs"] = json!({ "type": "salamander", "password": obfs_password });
            }
            if *up_mbps > 0 {
                o["up_mbps"] = json!(up_mbps);
            }
            if *down_mbps > 0 {
                o["down_mbps"] = json!(down_mbps);
            }
            o["tls"] = tls(n, true, &["h3"]).unwrap_or(Value::Null);
            return Ok(o);
        }
        Protocol::Tuic { uuid, password, congestion, udp_relay_mode } => {
            let mut o = json!({
                "type": "tuic", "server": n.server, "server_port": n.port, "uuid": uuid, "password": password,
                "congestion_control": or_default(congestion, "bbr"),
                "udp_relay_mode": or_default(udp_relay_mode, "native")
            });
            o["tls"] = tls(n, true, &["h3"]).unwrap_or(Value::Null);
            return Ok(o);
        }
        Protocol::Wireguard { private_key, peer_public_key, pre_shared_key, local_address, mtu, reserved } => {
            let mut peer = json!({
                "address": n.server, "port": n.port, "public_key": peer_public_key,
                "allowed_ips": ["0.0.0.0/0", "::/0"]
            });
            if !pre_shared_key.is_empty() {
                peer["pre_shared_key"] = json!(pre_shared_key);
            }
            if reserved.len() == 3 {
                peer["reserved"] = json!(reserved);
            }
            let addr: Vec<String> = if local_address.is_empty() { vec!["10.0.0.2/32".into()] } else { local_address.clone() };
            let mtu = if *mtu == 0 { 1408 } else { *mtu };
            return Ok(json!({
                "type": "wireguard", "address": addr, "private_key": private_key, "mtu": mtu, "peers": [peer]
            }));
        }
    };
    if let Some(t) = tls(n, false, &[]) {
        ob["tls"] = t;
    }
    if let Some(t) = transport(n)? {
        ob["transport"] = t;
    }
    Ok(ob)
}

/// TUN adapter: sing-box owns the TUN interface and fake-ip DNS and hands
/// every TCP connection and UDP flow to the hub's SOCKS inbound, so routing
/// decisions stay in the hub. The native TUN stack is the long-term replacement.
///
/// DNS on Windows is done by the system DNS Client service, so lookups made
/// by the engines themselves also arrive here as hijacked DNS. Node server
/// domains therefore get real answers from `direct-dns`, and the hub's own
/// "direct" connections enter through the `direct-in` SOCKS inbound so that
/// sing-box resolves them instead of the system.
/// Where the TUN adapter sends an application matched by a per-app rule.
/// The adapter sees which program opened a connection; the hub does not.
#[derive(Clone, Debug, PartialEq)]
pub enum FrontTarget {
    Direct,
    Block,
    /// Hand over to the hub (its rules and the selected node decide).
    Hub,
    /// Straight to a node's engine SOCKS inbound.
    Socks(u16),
}

/// Name variants sing-box may report for a Windows program ("Telegram" → "Telegram.exe", "telegram.exe" …).
fn process_variants(names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for n in names {
        let n = n.trim();
        if n.is_empty() {
            continue;
        }
        let base = n.strip_suffix(".exe").or_else(|| n.strip_suffix(".EXE")).unwrap_or(n);
        for v in [base.to_string(), format!("{base}.exe"), base.to_ascii_lowercase(), format!("{}.exe", base.to_ascii_lowercase())] {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn compile_tun(
    hub_port: u16,
    direct_port: u16,
    exclude_processes: &[String],
    server_domains: &[String],
    server_ips: &[std::net::IpAddr],
    direct_dns: &str,
    remote_dns: &str,
    block_quic: bool,
    app_rules: &[(Vec<String>, FrontTarget)],
) -> Value {
    let mut rules = vec![
        json!({ "inbound": ["direct-in"], "outbound": "direct" }),
        json!({ "action": "sniff" }),
        json!({ "process_name": exclude_processes, "outbound": "direct" }),
        json!({ "protocol": "dns", "action": "hijack-dns" }),
        json!({
            "ip_cidr": ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "169.254.0.0/16", "224.0.0.0/4", "255.255.255.255/32", "::1/128", "fe80::/10"],
            "outbound": "direct"
        }),
    ];
    let mut outbounds = vec![
        json!({ "type": "socks", "tag": "hub", "server": "127.0.0.1", "server_port": hub_port, "version": "5" }),
        json!({ "type": "direct", "tag": "direct" }),
    ];
    for (names, target) in app_rules {
        let names = process_variants(names);
        if names.is_empty() {
            continue;
        }
        let rule = match target {
            FrontTarget::Direct => json!({ "process_name": names, "outbound": "direct" }),
            FrontTarget::Block => json!({ "process_name": names, "action": "reject" }),
            FrontTarget::Hub => json!({ "process_name": names, "outbound": "hub" }),
            FrontTarget::Socks(port) => {
                let tag = format!("node-{port}");
                if !outbounds.iter().any(|o| o["tag"] == json!(tag.clone())) {
                    outbounds.push(json!({ "type": "socks", "tag": tag.clone(), "server": "127.0.0.1", "server_port": port, "version": "5" }));
                }
                json!({ "process_name": names, "outbound": tag })
            }
        };
        rules.push(rule);
    }
    if block_quic {
        rules.push(json!({ "network": "udp", "port": 443, "action": "reject" }));
    }
    let mut dns_rules = Vec::new();
    if !server_domains.is_empty() {
        dns_rules.push(json!({ "domain": server_domains, "server": "direct-dns" }));
    }
    dns_rules.push(json!({ "query_type": ["A", "AAAA"], "server": "fakeip" }));
    // Linux: the "system"/"mixed" stacks hand TCP to a listener on the TUN address, so
    // packets pass the INPUT firewall chain — ufw/firewalld (default on CachyOS, Fedora…)
    // drop them and nothing works. gVisor keeps the whole stack in userspace.
    let stack = if cfg!(target_os = "linux") { "gvisor" } else { "mixed" };
    let mut tun = json!({
        "type": "tun", "tag": "tun-in", "interface_name": "Mosaic",
        "address": ["172.19.0.1/30", "fdfe:dcba:9876::1/126"],
        "mtu": 9000, "auto_route": true, "strict_route": true, "stack": stack
    });
    // Node servers bypass the TUN at the routing level: loop-free even when the
    // process of an engine cannot be identified.
    if !server_ips.is_empty() {
        let cidrs: Vec<String> = server_ips.iter().map(|ip| if ip.is_ipv4() { format!("{ip}/32") } else { format!("{ip}/128") }).collect();
        tun["route_exclude_address"] = json!(cidrs);
    }
    json!({
        "log": { "level": "warn", "timestamp": false },
        "dns": {
            "servers": [
                { "type": "fakeip", "tag": "fakeip", "inet4_range": "198.18.0.0/15", "inet6_range": "fc00::/18" },
                { "type": "tcp", "tag": "remote", "server": remote_dns, "detour": "hub" },
                { "type": "udp", "tag": "direct-dns", "server": direct_dns }
            ],
            "rules": dns_rules,
            "final": "remote"
        },
        "inbounds": [
            tun,
            { "type": "socks", "tag": "direct-in", "listen": "127.0.0.1", "listen_port": direct_port }
        ],
        "outbounds": outbounds,
        "route": {
            "rules": rules,
            "final": "hub",
            "auto_detect_interface": true,
            "find_process": true,
            "default_domain_resolver": "direct-dns"
        }
    })
}
