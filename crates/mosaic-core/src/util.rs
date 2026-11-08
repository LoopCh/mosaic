use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decoding that never fails: invalid escapes are kept as-is.
/// `+` is kept literally (it is common inside base64 keys in share links).
pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Deterministic node id: the same link always gets the same id, so
/// re-importing or refreshing a subscription keeps selection and settings.
pub fn node_id(seed: &str) -> String {
    format!("{:016x}", fnv64(seed))
}

/// Bounded in-memory log shared by the hub and all engine processes.
pub struct LogBuf {
    lines: Mutex<VecDeque<String>>,
    cap: usize,
    echo: AtomicBool,
}

impl LogBuf {
    pub fn new(cap: usize) -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(cap)), cap, echo: AtomicBool::new(false) }
    }

    /// Also print every line to stderr (used by the CLI).
    pub fn set_echo(&self, on: bool) {
        self.echo.store(on, Ordering::Relaxed);
    }

    pub fn push(&self, line: impl Into<String>) {
        let line = line.into();
        if self.echo.load(Ordering::Relaxed) {
            eprintln!("{line}");
        }
        let mut g = self.lines.lock().unwrap();
        if g.len() >= self.cap {
            g.pop_front();
        }
        g.push_back(line);
    }

    pub fn all(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }

    pub fn tail_with(&self, needle: &str, n: usize) -> Vec<String> {
        let g = self.lines.lock().unwrap();
        let mut v: Vec<String> = g.iter().rev().filter(|l| l.contains(needle)).take(n).cloned().collect();
        v.reverse();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct() {
        assert_eq!(pct_decode("%D0%9F%D1%80%D0%B8%D0%B2%D0%B5%D1%82"), "Привет");
        assert_eq!(pct_decode("a%2Fb+c%"), "a/b+c%");
        assert_eq!(pct_decode("%zz"), "%zz");
    }

    #[test]
    fn ids_are_stable() {
        assert_eq!(node_id("vless://x"), node_id("vless://x"));
        assert_ne!(node_id("vless://x"), node_id("vless://y"));
    }
}
