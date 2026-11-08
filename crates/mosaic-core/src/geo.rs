//! Dependency-free reader for v2fly-format `geoip.dat` and `geosite.dat`
//! (the files shipped with Xray and most clients).
//!
//! Only the country codes / categories referenced by rules are decoded, so a
//! multi-megabyte geosite.dat costs a quick scan, not a full parse.
//!
//! Wire format (protobuf):
//!   GeoIPList   { repeated GeoIP entry = 1 }
//!   GeoIP       { string country_code = 1; repeated CIDR cidr = 2; bool reverse_match = 3 }
//!   CIDR        { bytes ip = 1; uint32 prefix = 2 }
//!   GeoSiteList { repeated GeoSite entry = 1 }
//!   GeoSite     { string country_code = 1; repeated Domain domain = 2 }
//!   Domain      { Type type = 1; string value = 2; ... }  Type: Plain=0 Regex=1 RootDomain=2 Full=3

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;

// ---------------------------------------------------------------- protobuf

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Skip,
}

struct Pb<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Pb<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i)?;
            self.i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// Next (field number, value); None at the end or on malformed input.
    fn next(&mut self) -> Option<(u32, Field<'a>)> {
        if self.i >= self.b.len() {
            return None;
        }
        let key = self.varint()?;
        let (num, wt) = ((key >> 3) as u32, key & 7);
        let f = match wt {
            0 => Field::Varint(self.varint()?),
            1 => {
                self.i = self.i.checked_add(8)?;
                Field::Skip
            }
            2 => {
                let len = self.varint()? as usize;
                let end = self.i.checked_add(len)?;
                let s = self.b.get(self.i..end)?;
                self.i = end;
                Field::Bytes(s)
            }
            5 => {
                self.i = self.i.checked_add(4)?;
                Field::Skip
            }
            _ => return None,
        };
        if self.i > self.b.len() {
            return None;
        }
        Some((num, f))
    }
}

/// Country code of an entry without decoding the rest of it.
fn entry_code(entry: &[u8]) -> Option<String> {
    let mut p = Pb::new(entry);
    while let Some((n, f)) = p.next() {
        if let (1, Field::Bytes(s)) = (n, f) {
            return Some(String::from_utf8_lossy(s).to_ascii_lowercase());
        }
    }
    None
}

// ---------------------------------------------------------------- GeoIP

/// Sorted, merged address ranges for fast binary-search lookups.
#[derive(Debug, Default, Clone)]
pub struct GeoIpSet {
    v4: Vec<(u32, u32)>,
    v6: Vec<(u128, u128)>,
    reverse: bool,
}

fn merge<T: Ord + Copy>(mut v: Vec<(T, T)>, succ: impl Fn(T) -> Option<T>) -> Vec<(T, T)> {
    v.sort();
    let mut out: Vec<(T, T)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        if let Some(last) = out.last_mut() {
            let adjacent = succ(last.1).map_or(true, |n| s <= n);
            if adjacent {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        out.push((s, e));
    }
    out
}

fn in_ranges<T: Ord + Copy>(r: &[(T, T)], x: T) -> bool {
    let idx = r.partition_point(|(s, _)| *s <= x);
    idx > 0 && x <= r[idx - 1].1
}

impl GeoIpSet {
    fn from_entry(entry: &[u8]) -> Self {
        let (mut v4, mut v6, mut reverse) = (Vec::new(), Vec::new(), false);
        let mut p = Pb::new(entry);
        while let Some((n, f)) = p.next() {
            match (n, f) {
                (2, Field::Bytes(cidr)) => {
                    let (mut ip, mut prefix) = (&[][..], 0u32);
                    let mut c = Pb::new(cidr);
                    while let Some((cn, cf)) = c.next() {
                        match (cn, cf) {
                            (1, Field::Bytes(b)) => ip = b,
                            (2, Field::Varint(v)) => prefix = v as u32,
                            _ => {}
                        }
                    }
                    if ip.len() == 4 {
                        let a = u32::from_be_bytes([ip[0], ip[1], ip[2], ip[3]]);
                        let p = prefix.min(32);
                        let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
                        v4.push((a & mask, (a & mask) | !mask));
                    } else if ip.len() == 16 {
                        let mut arr = [0u8; 16];
                        arr.copy_from_slice(ip);
                        let a = u128::from_be_bytes(arr);
                        let p = prefix.min(128);
                        let mask = if p == 0 { 0 } else { u128::MAX << (128 - p) };
                        v6.push((a & mask, (a & mask) | !mask));
                    }
                }
                (3, Field::Varint(v)) => reverse = v != 0,
                _ => {}
            }
        }
        Self { v4: merge(v4, |x: u32| x.checked_add(1)), v6: merge(v6, |x: u128| x.checked_add(1)), reverse }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let hit = match ip {
            IpAddr::V4(a) => in_ranges(&self.v4, u32::from(a)),
            IpAddr::V6(a) => match a.to_ipv4_mapped() {
                Some(v4) => in_ranges(&self.v4, u32::from(v4)),
                None => in_ranges(&self.v6, u128::from(a)),
            },
        };
        hit != self.reverse
    }

    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }
}

// ---------------------------------------------------------------- GeoSite

#[derive(Debug, Default, Clone)]
pub struct GeoSiteSet {
    full: HashSet<String>,
    root: HashSet<String>,
    keyword: Vec<String>,
    /// Regex entries are not supported yet (no regex engine in the core).
    pub skipped_regex: usize,
}

impl GeoSiteSet {
    fn from_entry(entry: &[u8]) -> Self {
        let mut s = GeoSiteSet::default();
        let mut p = Pb::new(entry);
        while let Some((n, f)) = p.next() {
            if let (2, Field::Bytes(dom)) = (n, f) {
                let (mut ty, mut value) = (0u64, String::new());
                let mut d = Pb::new(dom);
                while let Some((dn, df)) = d.next() {
                    match (dn, df) {
                        (1, Field::Varint(v)) => ty = v,
                        (2, Field::Bytes(b)) => value = String::from_utf8_lossy(b).to_ascii_lowercase(),
                        _ => {}
                    }
                }
                if value.is_empty() {
                    continue;
                }
                match ty {
                    0 => s.keyword.push(value),
                    1 => s.skipped_regex += 1,
                    2 => {
                        s.root.insert(value);
                    }
                    _ => {
                        s.full.insert(value);
                    }
                }
            }
        }
        s
    }

    /// `host` must be lower-case.
    pub fn contains(&self, host: &str) -> bool {
        if self.full.contains(host) {
            return true;
        }
        let mut h = host;
        loop {
            if self.root.contains(h) {
                return true;
            }
            match h.find('.') {
                Some(i) => h = &h[i + 1..],
                None => break,
            }
        }
        self.keyword.iter().any(|k| host.contains(k.as_str()))
    }

    pub fn len(&self) -> usize {
        self.full.len() + self.root.len() + self.keyword.len()
    }
}

// ---------------------------------------------------------------- loading

/// The decoded subsets referenced by the current rules.
#[derive(Debug, Default, Clone)]
pub struct GeoData {
    pub ip: HashMap<String, GeoIpSet>,
    pub site: HashMap<String, GeoSiteSet>,
}

impl GeoData {
    pub fn ip_contains(&self, code: &str, ip: IpAddr) -> bool {
        self.ip.get(&code.to_ascii_lowercase()).map_or(false, |s| s.contains(ip))
    }

    pub fn site_contains(&self, code: &str, host: &str) -> bool {
        // "google@ads" attribute filters are not supported; match the whole list.
        let code = code.split('@').next().unwrap_or(code).to_ascii_lowercase();
        self.site.get(&code).map_or(false, |s| s.contains(host))
    }
}

pub fn parse_geoip(data: &[u8], codes: &HashSet<String>) -> HashMap<String, GeoIpSet> {
    let mut out = HashMap::new();
    let mut p = Pb::new(data);
    while let Some((n, f)) = p.next() {
        if let (1, Field::Bytes(entry)) = (n, f) {
            if let Some(code) = entry_code(entry) {
                if codes.contains(&code) {
                    out.insert(code, GeoIpSet::from_entry(entry));
                }
            }
        }
    }
    out
}

pub fn parse_geosite(data: &[u8], codes: &HashSet<String>) -> HashMap<String, GeoSiteSet> {
    let mut out = HashMap::new();
    let mut p = Pb::new(data);
    while let Some((n, f)) = p.next() {
        if let (1, Field::Bytes(entry)) = (n, f) {
            if let Some(code) = entry_code(entry) {
                if codes.contains(&code) {
                    out.insert(code, GeoSiteSet::from_entry(entry));
                }
            }
        }
    }
    out
}

pub fn find_file(name: &str) -> Option<PathBuf> {
    crate::engines::search_dirs().into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Load only the requested codes. Returns the data plus human-readable warnings
/// (missing files, unknown codes) for the log.
pub fn load(ip_codes: &HashSet<String>, site_codes: &HashSet<String>) -> (GeoData, Vec<String>) {
    let mut warnings = Vec::new();
    let mut data = GeoData::default();
    let mut read = |name: &str| -> Option<Vec<u8>> {
        match find_file(name) {
            Some(p) => match std::fs::read(&p) {
                Ok(b) => Some(b),
                Err(e) => {
                    warnings.push(format!("cannot read {}: {e}", p.display()));
                    None
                }
            },
            None => {
                warnings.push(format!("{name} not found (put it into the engines folder; get-engines.ps1 downloads it)"));
                None
            }
        }
    };
    if !ip_codes.is_empty() {
        if let Some(b) = read("geoip.dat") {
            data.ip = parse_geoip(&b, ip_codes);
        }
    }
    if !site_codes.is_empty() {
        if let Some(b) = read("geosite.dat") {
            data.site = parse_geosite(&b, site_codes);
        }
    }
    for c in ip_codes {
        if !data.ip.contains_key(c) && find_file("geoip.dat").is_some() {
            warnings.push(format!("geoip:{c} is not in geoip.dat"));
        }
    }
    for c in site_codes {
        if !data.site.contains_key(c) && find_file("geosite.dat").is_some() {
            warnings.push(format!("geosite:{c} is not in geosite.dat"));
        }
    }
    (data, warnings)
}

pub fn file_status() -> (Option<String>, Option<String>) {
    let f = |n: &str| find_file(n).map(|p: PathBuf| p.display().to_string());
    (f("geoip.dat"), f("geosite.dat"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // tiny protobuf writer for tests
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }
    fn bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
        varint(((field as u64) << 3) | 2, out);
        varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }
    fn vint(field: u32, v: u64, out: &mut Vec<u8>) {
        varint((field as u64) << 3, out);
        varint(v, out);
    }

    fn cidr(ip: &[u8], prefix: u64) -> Vec<u8> {
        let mut c = Vec::new();
        bytes(1, ip, &mut c);
        vint(2, prefix, &mut c);
        c
    }

    #[test]
    fn geoip_roundtrip() {
        let mut xx = Vec::new();
        bytes(1, b"XX", &mut xx);
        bytes(2, &cidr(&[10, 0, 0, 0], 8), &mut xx);
        bytes(2, &cidr(&[11, 0, 0, 0], 8), &mut xx); // adjacent: merged with 10/8
        bytes(2, &cidr(&[203, 0, 113, 0], 24), &mut xx);
        let mut v6 = vec![0x20, 0x01, 0x0d, 0xb8];
        v6.extend_from_slice(&[0; 12]);
        bytes(2, &cidr(&v6, 32), &mut xx);
        let mut yy = Vec::new();
        bytes(1, b"YY", &mut yy);
        bytes(2, &cidr(&[1, 1, 1, 0], 24), &mut yy);
        let mut list = Vec::new();
        bytes(1, &xx, &mut list);
        bytes(1, &yy, &mut list);

        let codes: HashSet<String> = ["xx".to_string()].into_iter().collect();
        let m = parse_geoip(&list, &codes);
        assert!(m.get("yy").is_none(), "only requested codes are decoded");
        let s = &m["xx"];
        assert_eq!(s.v4.len(), 2);
        assert!(s.contains("10.1.2.3".parse().unwrap()));
        assert!(s.contains("11.255.255.255".parse().unwrap()));
        assert!(!s.contains("12.0.0.0".parse().unwrap()));
        assert!(s.contains("203.0.113.77".parse().unwrap()));
        assert!(s.contains("2001:db8::1".parse().unwrap()));
        assert!(!s.contains("2001:db9::1".parse().unwrap()));
        assert!(s.contains("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn geosite_roundtrip() {
        let dom = |ty: u64, v: &str| {
            let mut d = Vec::new();
            vint(1, ty, &mut d);
            bytes(2, v.as_bytes(), &mut d);
            d
        };
        let mut ads = Vec::new();
        bytes(1, b"CATEGORY-ADS", &mut ads);
        bytes(2, &dom(2, "doubleclick.net"), &mut ads);
        bytes(2, &dom(3, "ads.example.com"), &mut ads);
        bytes(2, &dom(0, "adservice"), &mut ads);
        bytes(2, &dom(1, "^ad[0-9]+\\."), &mut ads);
        let mut list = Vec::new();
        bytes(1, &ads, &mut list);

        let codes: HashSet<String> = ["category-ads".to_string()].into_iter().collect();
        let mut g = GeoData::default();
        g.site = parse_geosite(&list, &codes);
        assert!(g.site_contains("CATEGORY-ADS", "doubleclick.net"));
        assert!(g.site_contains("category-ads", "stats.g.doubleclick.net"));
        assert!(!g.site_contains("category-ads", "notdoubleclick.net"));
        assert!(g.site_contains("category-ads", "ads.example.com"));
        assert!(!g.site_contains("category-ads", "x.ads.example.com"));
        assert!(g.site_contains("category-ads@ads", "adservice.google.com"));
        assert_eq!(g.site["category-ads"].skipped_regex, 1);
        assert!(!g.site_contains("unknown", "doubleclick.net"));
    }

    #[test]
    fn malformed_input_does_not_panic() {
        let codes: HashSet<String> = ["xx".to_string()].into_iter().collect();
        assert!(parse_geoip(&[0x0a, 0xff, 0xff, 0xff], &codes).is_empty());
        assert!(parse_geosite(&[0xff; 7], &codes).is_empty());
    }
}
