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

/// One log record.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LogEntry {
    pub seq: u64,
    pub ts: u64,
    /// "error", "warn", "info" or "debug"
    pub level: &'static str,
    /// What wrote it: "hub", "tun", "Xray", "sing-box", "smart"… (the `[tag]` of the line).
    pub source: String,
    pub msg: String,
}

struct LogFile {
    path: std::path::PathBuf,
    file: std::fs::File,
    size: u64,
}

/// The log shared by the hub, the engines and the UI: a bounded in-memory ring for the
/// Logs screen, and (once `set_file` is called) a rotated file on disk that survives
/// crashes — `logs/mosaic.log`, then `.1` and `.2`, 2 MiB each.
pub struct LogBuf {
    ring: Mutex<(VecDeque<LogEntry>, u64)>,
    cap: usize,
    echo: AtomicBool,
    file: Mutex<Option<LogFile>>,
}

const LOG_FILE_MAX: u64 = 2 * 1024 * 1024;
const LOG_FILES_KEPT: usize = 3;

impl LogBuf {
    pub fn new(cap: usize) -> Self {
        Self { ring: Mutex::new((VecDeque::with_capacity(cap), 0)), cap, echo: AtomicBool::new(false), file: Mutex::new(None) }
    }

    /// Also print every line to stderr (used by the CLI).
    pub fn set_echo(&self, on: bool) {
        self.echo.store(on, Ordering::Relaxed);
    }

    /// Start writing to `dir/mosaic.log` (appending to what is there).
    pub fn set_file(&self, dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("mosaic.log");
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            *self.file.lock().unwrap() = Some(LogFile { path, file, size });
        }
    }

    pub fn push(&self, line: impl Into<String>) {
        let line = line.into();
        let (source, msg) = split_tag(&line);
        let level = classify(&source, &msg);
        self.record(level, source, msg);
    }

    pub fn warn(&self, line: impl Into<String>) {
        let line = line.into();
        let (source, msg) = split_tag(&line);
        self.record("warn", source, msg);
    }

    /// Detail lines (the connection journal with "verbose log" on).
    pub fn debug(&self, line: impl Into<String>) {
        let line = line.into();
        let (source, msg) = split_tag(&line);
        self.record("debug", source, msg);
    }

    /// The log file (`logs/mosaic.log`), once `set_file` was called. The Linux TUN helper
    /// appends its own lines to it — they matter most when Mosaic itself is gone.
    pub fn file_path(&self) -> Option<std::path::PathBuf> {
        self.file.lock().unwrap().as_ref().map(|f| f.path.clone())
    }

    pub fn error(&self, line: impl Into<String>) {
        let line = line.into();
        let (source, msg) = split_tag(&line);
        self.record("error", source, msg);
    }

    fn record(&self, level: &'static str, source: String, msg: String) {
        let ts = now_ms();
        if self.echo.load(Ordering::Relaxed) {
            eprintln!("[{source}] {msg}");
        }
        self.write_file(ts, level, &source, &msg);
        let mut g = self.ring.lock().unwrap();
        g.1 += 1;
        let seq = g.1;
        if g.0.len() >= self.cap {
            g.0.pop_front();
        }
        g.0.push_back(LogEntry { seq, ts, level, source, msg });
    }

    fn write_file(&self, ts: u64, level: &str, source: &str, msg: &str) {
        use std::io::Write;
        let mut g = self.file.lock().unwrap();
        let Some(f) = g.as_mut() else { return };
        let line = format!("{} {:<5} [{source}] {msg}\n", fmt_utc(ts), level.to_uppercase());
        if f.file.write_all(line.as_bytes()).is_ok() {
            f.size += line.len() as u64;
        }
        if f.size >= LOG_FILE_MAX {
            // mosaic.log → mosaic.1.log → mosaic.2.log (the oldest is dropped)
            let base = f.path.clone();
            let name = |i: usize| base.with_file_name(format!("mosaic.{i}.log"));
            for i in (1..LOG_FILES_KEPT).rev() {
                let from = if i == 1 { base.clone() } else { name(i - 1) };
                let _ = std::fs::rename(&from, name(i));
            }
            if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&base) {
                *f = LogFile { path: base, file, size: 0 };
            }
        }
    }

    /// Lines as text ("[source] message"), oldest first.
    pub fn all(&self) -> Vec<String> {
        self.ring.lock().unwrap().0.iter().map(|e| format!("[{}] {}", e.source, e.msg)).collect()
    }

    /// Records newer than `seq` (0: everything kept).
    pub fn since(&self, seq: u64) -> Vec<LogEntry> {
        self.ring.lock().unwrap().0.iter().filter(|e| e.seq > seq).cloned().collect()
    }

    pub fn tail_with(&self, needle: &str, n: usize) -> Vec<String> {
        let g = self.ring.lock().unwrap();
        let mut v: Vec<String> =
            g.0.iter().rev().map(|e| format!("[{}] {}", e.source, e.msg)).filter(|l| l.contains(needle)).take(n).collect();
        v.reverse();
        v
    }
}

/// "Say this at most once per window": repeated events (the same STUN server, the same
/// failing host) leave one line in the log instead of hundreds.
#[derive(Default)]
pub struct Throttle(Mutex<std::collections::HashMap<String, u64>>);

impl Throttle {
    pub fn allow(&self, key: &str, window_ms: u64) -> bool {
        let now = now_ms();
        let mut g = self.0.lock().unwrap();
        if g.len() > 2000 {
            g.retain(|_, t| now.saturating_sub(*t) < window_ms);
        }
        match g.get(key) {
            Some(t) if now.saturating_sub(*t) < window_ms => false,
            _ => {
                g.insert(key.to_string(), now);
                true
            }
        }
    }
}

/// "[tun] packets in …" → ("tun", "packets in …"); untagged lines are "app".
fn split_tag(line: &str) -> (String, String) {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            if end > 0 && end <= 24 {
                return (rest[..end].to_string(), rest[end + 1..].trim_start().to_string());
            }
        }
    }
    ("app".to_string(), line.to_string())
}

/// Level of a line: engines mark theirs ("[Warning]" in Xray, "WARN" in sing-box); for
/// Mosaic's own lines a few words are enough.
fn classify(source: &str, msg: &str) -> &'static str {
    let engine = source.starts_with("Xray") || source.starts_with("sing-box") || source.contains("adapter");
    if engine {
        for (needle, level) in [
            ("[Error]", "error"),
            ("[Warning]", "warn"),
            ("[Debug]", "debug"),
            ("FATAL", "error"),
            ("ERROR", "error"),
            ("WARN", "warn"),
            ("DEBUG", "debug"),
            ("TRACE", "debug"),
        ] {
            if msg.contains(needle) {
                return level;
            }
        }
        return "info";
    }
    if source == "panic" {
        return "error";
    }
    let m = msg.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if m.starts_with("error") || has(&["failed", "could not", "cannot ", "can't ", "panic", "not found", "denied", "refused", "stopped:"]) {
        "error"
    } else if m.starts_with("warn") || has(&["not responding", "timed out", "timeout", "stopped answering", "skipped", "warning", "not answer", "retry", "fallback"]) {
        "warn"
    } else {
        "info"
    }
}

/// 2026-11-10T05:38:12.345Z
pub fn fmt_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", sod / 3600, sod / 60 % 60, sod % 60, ms % 1000)
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

    #[test]
    fn log_levels_and_dates() {
        assert_eq!(fmt_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(fmt_utc(1_762_752_000_123), "2025-11-10T05:20:00.123Z");
        assert_eq!(split_tag("[tun] hello"), ("tun".to_string(), "hello".to_string()));
        assert_eq!(classify("Xray", "2026/11/10 [Warning] core: x"), "warn");
        assert_eq!(classify("sing-box", "ERROR[0000] start service"), "error");
        assert_eq!(classify("check", "node is not responding: x"), "warn");
        assert_eq!(classify("hub", "connected"), "info");
        let l = LogBuf::new(3);
        for i in 0..5 {
            l.push(format!("[t] {i}"));
        }
        assert_eq!(l.since(0).len(), 3);
        assert_eq!(l.since(4).len(), 1);
    }
}

/// Raise the soft limit on open files to the hard limit. The hub lives in the app
/// process (next to WebKit), and in TUN mode every connection holds a socket, so the
/// usual soft limit of 1024 runs out ("Too many open files"). Go programs (Xray,
/// sing-box) do the same at start. Returns (old, new) soft limits.
#[cfg(unix)]
pub fn raise_fd_limit() -> Option<(u64, u64)> {
    unsafe {
        let mut l: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) != 0 {
            return None;
        }
        let old = l.rlim_cur as u64;
        // Some kernels report an "infinite" hard limit; keep the new value sane.
        let want = if l.rlim_max == libc::RLIM_INFINITY { 1 << 20 } else { l.rlim_max };
        if l.rlim_cur >= want {
            return Some((old, old));
        }
        l.rlim_cur = want;
        if libc::setrlimit(libc::RLIMIT_NOFILE, &l) != 0 {
            return Some((old, old));
        }
        Some((old, want as u64))
    }
}

#[cfg(not(unix))]
pub fn raise_fd_limit() -> Option<(u64, u64)> {
    None
}
