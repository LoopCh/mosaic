//! Diagnostics: a report to attach when something does not work.
//!
//! The report is meant to be shared (an issue, a chat), so everything that identifies the
//! user's servers and accounts is replaced before it leaves: node names, server addresses,
//! UUIDs / passwords / keys, share links, subscription URLs and titles, and public IP
//! addresses in log lines. Private, loopback and Mosaic's own addresses (fake IPs, the TUN)
//! stay — they are what makes a network problem readable.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::State;

/// Pairs (secret, placeholder), longest first so that a URL is replaced before its host.
pub fn secrets(st: &State) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    for (i, s) in st.subscriptions.iter().enumerate() {
        v.push((s.url.clone(), format!("<subscription-{}>", i + 1)));
        if let Some(h) = url_host(&s.url) {
            v.push((h, format!("<subscription-host-{}>", i + 1)));
        }
        v.push((s.name.clone(), format!("<subscription-name-{}>", i + 1)));
    }
    for (i, n) in st.nodes.iter().enumerate() {
        v.push((n.link.clone(), format!("<link-{}>", i + 1)));
        v.push((n.name.clone(), format!("<node-{}>", i + 1)));
        v.push((n.server.clone(), format!("<server-{}>", i + 1)));
        let j = serde_json::to_value(n).unwrap_or(Value::Null);
        for key in ["protocol", "transport", "security"] {
            collect_strings(&j[key], &mut |s| {
                if s.len() >= 8 {
                    v.push((s.to_string(), "<secret>".into()));
                }
            });
        }
    }
    v.retain(|(k, _)| k.chars().count() >= 3);
    v.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

fn collect_strings(v: &Value, f: &mut dyn FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => {
            for x in a {
                collect_strings(x, &mut *f);
            }
        }
        Value::Object(o) => {
            for x in o.values() {
                collect_strings(x, &mut *f);
            }
        }
        _ => {}
    }
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    let host = if host.starts_with('[') { host.split(']').next()?.trim_start_matches('[') } else { host.split(':').next()? };
    (!host.is_empty()).then(|| host.to_string())
}

pub fn redact(text: &str, secrets: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (s, p) in secrets {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), p);
        }
    }
    redact_public_ipv4(&out)
}

/// Public IPv4 addresses → "<ip>"; private, loopback, link-local, CGNAT, the fake-IP range
/// (198.18.0.0/15) and documentation ranges stay.
fn redact_public_ipv4(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let starts = b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.'));
        if starts {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            let cand = text[i..j].trim_end_matches('.');
            let end = i + cand.len();
            let followed_ok = end >= b.len() || !b[end].is_ascii_alphanumeric();
            if let (Ok(ip), true) = (cand.parse::<Ipv4Addr>(), followed_ok) {
                if keep_ip(ip) {
                    out.push_str(cand);
                } else {
                    out.push_str("<ip>");
                }
                i = end;
                continue;
            }
            out.push_str(&text[i..j]);
            i = j;
            continue;
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn keep_ip(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || (o[0] == 100 && (o[1] & 0xC0) == 64)
        || (o[0] == 198 && (o[1] & 0xFE) == 18)
        // public DNS resolvers say which DNS is used, not who the user is
        || matches!(o, [1, 1, 1, 1] | [1, 0, 0, 1] | [8, 8, 8, 8] | [8, 8, 4, 4] | [9, 9, 9, 9] | [149, 112, 112, 112] | [208, 67, 222, 222] | [94, 140, 14, 14])
}

/// The operating system, as precisely as is cheap to find out.
pub fn os_description() -> String {
    let base = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    match os_details() {
        Some(d) => format!("{base} · {d}"),
        None => base,
    }
}

#[cfg(target_os = "linux")]
fn os_details() -> Option<String> {
    let pretty = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())));
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    Some(format!("{} · kernel {kernel} · {desktop} {session}", pretty.unwrap_or_else(|| "Linux".into())))
}

#[cfg(windows)]
fn os_details() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("cmd").args(["/c", "ver"]).creation_flags(0x0800_0000).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn os_details() -> Option<String> {
    None
}

/// Where a saved report goes: the user's Downloads folder (XDG on Linux, so a localised
/// "Загрузки" is found too), else `fallback`.
pub fn downloads_dir(fallback: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Ok(p) = std::env::var("USERPROFILE") {
        let d = PathBuf::from(p).join("Downloads");
        if d.is_dir() {
            return d;
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(home) = std::env::var("HOME") {
        let cfg = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(&home).join(".config"));
        if let Ok(s) = std::fs::read_to_string(cfg.join("user-dirs.dirs")) {
            if let Some(v) = s.lines().find_map(|l| l.strip_prefix("XDG_DOWNLOAD_DIR=")) {
                let d = PathBuf::from(v.trim_matches('"').replace("$HOME", &home));
                if d.is_dir() {
                    return d;
                }
            }
        }
        let d = PathBuf::from(&home).join("Downloads");
        if d.is_dir() {
            return d;
        }
    }
    fallback.to_path_buf()
}

/// Open a folder in the file manager.
pub fn open_folder(path: &Path) -> Result<(), String> {
    let _ = path;
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(not(any(windows, target_os = "linux")))]
    let r: std::io::Result<std::process::Child> = Err(std::io::Error::other("not supported here"));
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ips() {
        assert_eq!(
            redact_public_ipv4("tcp 172.19.0.1:40452 → 46.29.118.25:443, dns 8.8.8.8, fake 198.18.0.5, v1.2.3.4x"),
            "tcp 172.19.0.1:40452 → <ip>:443, dns 8.8.8.8, fake 198.18.0.5, v1.2.3.4x"
        );
        assert_eq!(redact_public_ipv4("Xray 26.11.9 started"), "Xray 26.11.9 started");
        assert_eq!(redact_public_ipv4("ip 203.0.113.7."), "ip 203.0.113.7.");
        assert_eq!(redact_public_ipv4("Привет 5.6.7.8!"), "Привет <ip>!");
    }

    #[test]
    fn hosts() {
        assert_eq!(url_host("https://user@sub.example.com:8443/a?b").as_deref(), Some("sub.example.com"));
        assert_eq!(url_host("https://[2001:db8::1]/x").as_deref(), Some("2001:db8::1"));
    }
}
