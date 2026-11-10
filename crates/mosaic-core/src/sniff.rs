//! Domain sniffing from the first bytes of a connection: TLS ClientHello SNI
//! and HTTP/1 Host header. Used when the destination is a bare IP.

pub fn sniff(buf: &[u8]) -> Option<String> {
    tls_sni(buf).or_else(|| http_host(buf))
}

/// A STUN message (RFC 5389): WebRTC uses them to discover the public address.
/// Header: two zero bits, type, length (multiple of 4), magic cookie 0x2112A442.
pub fn is_stun(b: &[u8]) -> bool {
    if b.len() < 20 || b[0] & 0xC0 != 0 {
        return false;
    }
    let len = u16::from_be_bytes([b[2], b[3]]) as usize;
    len % 4 == 0 && len + 20 == b.len() && b[4..8] == [0x21, 0x12, 0xA4, 0x42]
}

pub fn tls_sni(b: &[u8]) -> Option<String> {
    // Record header: type 22 (handshake), version, length
    if b.len() < 5 || b[0] != 0x16 || b[1] != 0x03 {
        return None;
    }
    let mut p = 5;
    // Handshake header: type 1 (ClientHello), 3-byte length
    if *b.get(p)? != 0x01 {
        return None;
    }
    p += 4;
    p += 2 + 32; // client version + random
    let sid = *b.get(p)? as usize;
    p += 1 + sid;
    let cs = u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]) as usize;
    p += 2 + cs;
    let cm = *b.get(p)? as usize;
    p += 1 + cm;
    let ext_total = u16::from_be_bytes([*b.get(p)?, *b.get(p + 1)?]) as usize;
    p += 2;
    let end = (p + ext_total).min(b.len());
    while p + 4 <= end {
        let ty = u16::from_be_bytes([b[p], b[p + 1]]);
        let len = u16::from_be_bytes([b[p + 2], b[p + 3]]) as usize;
        p += 4;
        if ty == 0 {
            // server_name: list length (2), entry type (1), name length (2), name
            let name_type = *b.get(p + 2)?;
            let nl = u16::from_be_bytes([*b.get(p + 3)?, *b.get(p + 4)?]) as usize;
            if name_type != 0 {
                return None;
            }
            let name = b.get(p + 5..p + 5 + nl)?;
            let s = std::str::from_utf8(name).ok()?.to_ascii_lowercase();
            return valid_host(&s).then_some(s);
        }
        p += len;
    }
    None
}

pub fn http_host(b: &[u8]) -> Option<String> {
    let methods: [&[u8]; 9] = [b"GET ", b"POST ", b"HEAD ", b"PUT ", b"DELETE ", b"OPTIONS ", b"PATCH ", b"CONNECT ", b"TRACE "];
    if !methods.iter().any(|m| b.starts_with(m)) {
        return None;
    }
    let text = String::from_utf8_lossy(&b[..b.len().min(4096)]);
    for line in text.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("host") {
                let v = v.trim();
                let host = if let Some(r) = v.strip_prefix('[') {
                    r.split(']').next().unwrap_or("")
                } else {
                    v.rsplit_once(':').map_or(v, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { v })
                };
                let host = host.to_ascii_lowercase();
                return valid_host(&host).then_some(host);
            }
        }
    }
    None
}

fn valid_host(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b'_' || c == b':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_hello(sni: &str) -> Vec<u8> {
        let name = sni.as_bytes();
        let mut sni_ext = Vec::new();
        sni_ext.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes());
        sni_ext.push(0);
        sni_ext.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sni_ext.extend_from_slice(name);
        let mut exts = Vec::new();
        // an unrelated extension first (supported_groups)
        exts.extend_from_slice(&[0x00, 0x0a, 0x00, 0x04, 0x00, 0x02, 0x00, 0x1d]);
        exts.extend_from_slice(&[0x00, 0x00]);
        exts.extend_from_slice(&(sni_ext.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sni_ext);
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[7u8; 32]);
        body.push(32);
        body.extend_from_slice(&[9u8; 32]);
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]);
        body.extend_from_slice(&[0x01, 0x00]);
        body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        body.extend_from_slice(&exts);
        let mut hs = vec![0x01];
        hs.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn sni() {
        assert_eq!(sniff(&client_hello("Www.Example.COM")), Some("www.example.com".into()));
        let ch = client_hello("example.org");
        assert_eq!(tls_sni(&ch[..ch.len() - 5]), None, "truncated hello");
        assert_eq!(tls_sni(b"\x16\x03\x01\x00"), None);
    }

    #[test]
    fn http() {
        assert_eq!(sniff(b"GET / HTTP/1.1\r\nUser-Agent: x\r\nHost: Example.net:8080\r\n\r\n"), Some("example.net".into()));
        assert_eq!(http_host(b"POST /a HTTP/1.1\r\nhost: [2001:db8::1]:80\r\n\r\n"), Some("2001:db8::1".into()));
        assert_eq!(sniff(b"SSH-2.0-OpenSSH\r\n"), None);
    }

    #[test]
    fn stun() {
        // Binding request, no attributes.
        let mut m = vec![0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42];
        m.extend_from_slice(&[7u8; 12]);
        assert!(super::is_stun(&m));
        m[4] = 0;
        assert!(!super::is_stun(&m));
        assert!(!super::is_stun(&[0x16, 0x03, 0x01, 0, 0]));
    }
}
