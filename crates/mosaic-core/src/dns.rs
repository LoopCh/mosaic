//! Minimal DNS message codec (A/AAAA) and a fake-IP pool. Pure code, no I/O.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;

#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

pub fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut m = Vec::with_capacity(32 + name.len());
    m.extend_from_slice(&id.to_be_bytes());
    m.extend_from_slice(&[0x01, 0x00]); // RD
    m.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QD=1
    for label in name.trim_end_matches('.').split('.') {
        let l = label.as_bytes();
        m.push(l.len().min(63) as u8);
        m.extend_from_slice(&l[..l.len().min(63)]);
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&[0, 1]);
    m
}

/// Read a (possibly compressed) name starting at `pos`. Returns the name and
/// the position right after it in the original byte stream.
fn read_name(msg: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end: Option<usize> = None;
    let mut jumps = 0;
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 == 0xc0 {
            let ptr = ((len & 0x3f) << 8) | *msg.get(pos + 1)? as usize;
            if end.is_none() {
                end = Some(pos + 2);
            }
            jumps += 1;
            if jumps > 32 {
                return None;
            }
            pos = ptr;
            continue;
        }
        let label = msg.get(pos + 1..pos + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        pos += 1 + len;
    }
    Some((labels.join("."), end.unwrap_or(pos)))
}

/// Parse the first question. Returns (id, question, offset after the question).
pub fn parse_query(msg: &[u8]) -> Option<(u16, Question, usize)> {
    if msg.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let qd = u16::from_be_bytes([msg[4], msg[5]]);
    if qd == 0 {
        return None;
    }
    let (name, p) = read_name(msg, 12)?;
    let qtype = u16::from_be_bytes([*msg.get(p)?, *msg.get(p + 1)?]);
    let qclass = u16::from_be_bytes([*msg.get(p + 2)?, *msg.get(p + 3)?]);
    Some((id, Question { name, qtype, qclass }, p + 4))
}

/// Parse a response: (id, rcode, addresses with TTL).
pub fn parse_response(msg: &[u8]) -> Option<(u16, u8, Vec<(IpAddr, u32)>)> {
    if msg.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let rcode = msg[3] & 0x0f;
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;
    let mut p = 12;
    for _ in 0..qd {
        let (_, np) = read_name(msg, p)?;
        p = np + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        let (_, np) = read_name(msg, p)?;
        p = np;
        let ty = u16::from_be_bytes([*msg.get(p)?, *msg.get(p + 1)?]);
        let ttl = u32::from_be_bytes([*msg.get(p + 4)?, *msg.get(p + 5)?, *msg.get(p + 6)?, *msg.get(p + 7)?]);
        let rdlen = u16::from_be_bytes([*msg.get(p + 8)?, *msg.get(p + 9)?]) as usize;
        let rdata = msg.get(p + 10..p + 10 + rdlen)?;
        match (ty, rdlen) {
            (TYPE_A, 4) => out.push((IpAddr::V4(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3])), ttl)),
            (TYPE_AAAA, 16) => {
                let mut a = [0u8; 16];
                a.copy_from_slice(rdata);
                out.push((IpAddr::V6(Ipv6Addr::from(a)), ttl));
            }
            _ => {}
        }
        p += 10 + rdlen;
    }
    Some((id, rcode, out))
}

/// Build a response to `query` (whose question ends at `q_end`) with the given
/// addresses. Addresses of the wrong family for the question type are skipped.
pub fn build_response(query: &[u8], q_end: usize, qtype: u16, answers: &[IpAddr], ttl: u32, rcode: u8) -> Vec<u8> {
    let picked: Vec<&IpAddr> = answers
        .iter()
        .filter(|a| matches!((a, qtype), (IpAddr::V4(_), TYPE_A) | (IpAddr::V6(_), TYPE_AAAA)))
        .collect();
    let mut m = Vec::with_capacity(q_end + picked.len() * 28);
    m.extend_from_slice(&query[0..2]);
    let rd = query[2] & 0x01;
    m.push(0x80 | rd); // QR, opcode 0, RD copied
    m.push(0x80 | (rcode & 0x0f)); // RA
    m.extend_from_slice(&[0, 1]);
    m.extend_from_slice(&(picked.len() as u16).to_be_bytes());
    m.extend_from_slice(&[0, 0, 0, 0]);
    m.extend_from_slice(&query[12..q_end]);
    for a in picked {
        m.extend_from_slice(&[0xc0, 0x0c]); // pointer to the question name
        m.extend_from_slice(&qtype.to_be_bytes());
        m.extend_from_slice(&[0, 1]);
        m.extend_from_slice(&ttl.to_be_bytes());
        match a {
            IpAddr::V4(v) => {
                m.extend_from_slice(&[0, 4]);
                m.extend_from_slice(&v.octets());
            }
            IpAddr::V6(v) => {
                m.extend_from_slice(&[0, 16]);
                m.extend_from_slice(&v.octets());
            }
        }
    }
    m
}

// ---------------------------------------------------------------- fake IP

/// Hands out addresses from 198.18.0.0/15 and remembers which name each one
/// stands for. When the pool wraps around, the oldest mappings are reused.
pub struct FakeIpPool {
    base: u32,
    size: u32,
    next: u32,
    by_ip: HashMap<u32, String>,
    by_name: HashMap<String, u32>,
}

impl Default for FakeIpPool {
    fn default() -> Self {
        Self::new(Ipv4Addr::new(198, 18, 0, 0), 15)
    }
}

impl FakeIpPool {
    pub fn new(base: Ipv4Addr, prefix: u32) -> Self {
        // Skip .0 and .1 of the range.
        Self { base: u32::from(base), size: 1u32 << (32 - prefix), next: 2, by_ip: HashMap::new(), by_name: HashMap::new() }
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let v = u32::from(ip);
        v >= self.base && v < self.base + self.size
    }

    pub fn alloc(&mut self, name: &str) -> Ipv4Addr {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if let Some(&off) = self.by_name.get(&name) {
            return Ipv4Addr::from(self.base + off);
        }
        let off = self.next;
        self.next = if self.next + 1 >= self.size - 1 { 2 } else { self.next + 1 };
        if let Some(old) = self.by_ip.insert(off, name.clone()) {
            self.by_name.remove(&old);
        }
        self.by_name.insert(name, off);
        Ipv4Addr::from(self.base + off)
    }

    pub fn lookup(&self, ip: Ipv4Addr) -> Option<&str> {
        if !self.contains(ip) {
            return None;
        }
        self.by_ip.get(&(u32::from(ip) - self.base)).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_roundtrip_and_response() {
        let q = build_query(0x1234, "www.Example.com.", TYPE_A);
        let (id, question, end) = parse_query(&q).unwrap();
        assert_eq!(id, 0x1234);
        assert_eq!(question, Question { name: "www.example.com".into(), qtype: TYPE_A, qclass: 1 });
        assert_eq!(end, q.len());

        let answers: Vec<IpAddr> = vec!["93.184.216.34".parse().unwrap(), "2001:db8::1".parse().unwrap()];
        let r = build_response(&q, end, TYPE_A, &answers, 60, 0);
        let (rid, rcode, addrs) = parse_response(&r).unwrap();
        assert_eq!((rid, rcode), (0x1234, 0));
        assert_eq!(addrs, vec![("93.184.216.34".parse().unwrap(), 60)]);

        let r6 = build_response(&q, end, TYPE_AAAA, &answers, 30, 0);
        let (_, _, a6) = parse_response(&r6).unwrap();
        assert_eq!(a6, vec![("2001:db8::1".parse().unwrap(), 30)]);
    }

    #[test]
    fn compressed_names_in_answers() {
        // A real-world shaped answer: CNAME then A, names compressed.
        let q = build_query(7, "a.example.org", TYPE_A);
        let (_, _, end) = parse_query(&q).unwrap();
        let mut r = q[..end].to_vec();
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 2; // ANCOUNT
        // CNAME a.example.org -> b.example.org (b + pointer to "example.org" at offset 14)
        r.extend_from_slice(&[0xc0, 0x0c, 0, 5, 0, 1, 0, 0, 0, 10, 0, 4, 1, b'b', 0xc0, 14]);
        let cname_target = r.len() - 4;
        // A for b.example.org
        r.extend_from_slice(&[0xc0, cname_target as u8, 0, 1, 0, 1, 0, 0, 1, 0, 0, 4, 10, 0, 0, 7]);
        let (_, _, addrs) = parse_response(&r).unwrap();
        assert_eq!(addrs, vec![("10.0.0.7".parse().unwrap(), 256)]);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_query(&[1, 2, 3]).is_none());
        assert!(parse_response(&[0; 11]).is_none());
        let mut loopy = build_query(1, "x", TYPE_A);
        loopy[12] = 0xc0;
        loopy[13] = 12; // pointer to itself
        assert!(parse_query(&loopy).is_none());
    }

    #[test]
    fn fake_ip_pool() {
        let mut p = FakeIpPool::new(Ipv4Addr::new(198, 18, 0, 0), 29); // 8 addresses, 2..6 usable
        let a = p.alloc("a.com");
        assert_eq!(a, Ipv4Addr::new(198, 18, 0, 2));
        assert_eq!(p.alloc("A.com."), a);
        assert_eq!(p.lookup(a), Some("a.com"));
        for n in ["b", "c", "d", "e"] {
            p.alloc(n);
        }
        // wrapped: a.com's slot reused
        let f = p.alloc("f");
        assert_eq!(f, a);
        assert_eq!(p.lookup(a), Some("f"));
        assert!(p.lookup(Ipv4Addr::new(8, 8, 8, 8)).is_none());
    }
}
