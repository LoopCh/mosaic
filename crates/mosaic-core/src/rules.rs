//! Rule matching. Rules are checked top to bottom; first match wins.

use std::net::IpAddr;

use crate::model::{Rule, RuleKind};

pub const PRIVATE_CIDRS: &str =
    "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,127.0.0.0/8,169.254.0.0/16,100.64.0.0/10,::1/128,fc00::/7,fe80::/10";

pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule { kind: RuleKind::IpCidr, value: PRIVATE_CIDRS.into(), target: "direct".into(), enabled: true },
        Rule { kind: RuleKind::DomainSuffix, value: "local,localhost,lan".into(), target: "direct".into(), enabled: true },
    ]
}

/// `host` must already be lower-case. `ip` is `host` parsed as an IP, if it is one.
pub fn matches(rule: &Rule, host: &str, ip: Option<IpAddr>, port: u16) -> bool {
    rule.value
        .split(',')
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .any(|v| match rule.kind {
            RuleKind::Domain => host.eq_ignore_ascii_case(v),
            RuleKind::DomainSuffix => {
                let v = v.trim_start_matches('.').to_ascii_lowercase();
                host == v || (host.len() > v.len() && host.ends_with(&v) && host.as_bytes()[host.len() - v.len() - 1] == b'.')
            }
            RuleKind::DomainKeyword => host.contains(&v.to_ascii_lowercase()),
            RuleKind::IpCidr => ip.map_or(false, |ip| cidr_contains(v, ip)),
            RuleKind::Port => port_match(v, port),
        })
}

pub fn cidr_contains(cidr: &str, ip: IpAddr) -> bool {
    let (net, len) = match cidr.split_once('/') {
        Some((n, l)) => match l.trim().parse::<u32>() {
            Ok(l) => (n.trim(), Some(l)),
            Err(_) => return false,
        },
        None => (cidr.trim(), None),
    };
    let Ok(net) = net.parse::<IpAddr>() else { return false };
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(a)) => {
            let len = len.unwrap_or(32).min(32);
            if len == 0 {
                return true;
            }
            let mask = u32::MAX << (32 - len);
            (u32::from(n) & mask) == (u32::from(a) & mask)
        }
        (IpAddr::V6(n), IpAddr::V6(a)) => {
            let len = len.unwrap_or(128).min(128);
            if len == 0 {
                return true;
            }
            let mask = u128::MAX << (128 - len);
            (u128::from(n) & mask) == (u128::from(a) & mask)
        }
        _ => false,
    }
}

fn port_match(v: &str, port: u16) -> bool {
    match v.split_once('-') {
        Some((a, b)) => match (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
            (Ok(a), Ok(b)) => port >= a && port <= b,
            _ => false,
        },
        None => v.parse::<u16>().map_or(false, |p| p == port),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(kind: RuleKind, v: &str) -> Rule {
        Rule { kind, value: v.into(), target: "direct".into(), enabled: true }
    }

    #[test]
    fn suffix() {
        let rule = r(RuleKind::DomainSuffix, ".ru, xn--p1ai");
        assert!(matches(&rule, "yandex.ru", None, 443));
        assert!(matches(&rule, "ru", None, 443));
        assert!(matches(&rule, "xn--80ak6aa92e.xn--p1ai", None, 443));
        assert!(!matches(&rule, "guru", None, 443));
        assert!(!matches(&rule, "example.com", None, 443));
    }

    #[test]
    fn cidr_and_port() {
        let rule = r(RuleKind::IpCidr, PRIVATE_CIDRS);
        assert!(matches(&rule, "192.168.1.10", "192.168.1.10".parse().ok(), 80));
        assert!(matches(&rule, "fd12::1", "fd12::1".parse().ok(), 80));
        assert!(!matches(&rule, "8.8.8.8", "8.8.8.8".parse().ok(), 80));
        assert!(!matches(&rule, "example.com", None, 80));
        assert!(cidr_contains("0.0.0.0/0", "1.2.3.4".parse().unwrap()));
        let p = r(RuleKind::Port, "22, 6000-7000");
        assert!(matches(&p, "x", None, 22));
        assert!(matches(&p, "x", None, 6500));
        assert!(!matches(&p, "x", None, 443));
    }

    #[test]
    fn keyword_and_exact() {
        assert!(matches(&r(RuleKind::DomainKeyword, "google"), "www.google.com", None, 1));
        assert!(matches(&r(RuleKind::Domain, "Example.com"), "example.com", None, 1));
        assert!(!matches(&r(RuleKind::Domain, "example.com"), "a.example.com", None, 1));
    }
}
