//! Lenient base64 decoder: standard and URL-safe alphabets, padding optional,
//! whitespace ignored. Subscriptions in the wild use every combination.

pub fn decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4 + 3);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for c in input.bytes() {
        let v: u32 = match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1u32 << bits) - 1;
        }
    }
    Some(out)
}

pub fn decode_str(input: &str) -> Option<String> {
    String::from_utf8(decode(input)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants() {
        assert_eq!(decode_str("aGVsbG8gd29ybGQ=").unwrap(), "hello world");
        assert_eq!(decode_str("aGVsbG8gd29ybGQ").unwrap(), "hello world");
        assert_eq!(decode_str("YWVzLTI1Ni1nY206cGFzcw").unwrap(), "aes-256-gcm:pass");
        assert_eq!(decode(">>").is_none(), true);
        // url-safe
        assert_eq!(decode("-_8").unwrap(), vec![0xfb, 0xff]);
    }
}
