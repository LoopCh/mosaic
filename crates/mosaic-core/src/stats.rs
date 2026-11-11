//! Traffic counters and the connection journal ("why did this go there?").

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

use crate::util::{now_ms, LogBuf, Throttle};

const CLOSED_KEEP: usize = 300;

pub struct ConnEntry {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub network: &'static str,
    pub process: Option<String>,
    pub rule: String,
    pub outbound: String,
    pub engine: Option<String>,
    pub started_ms: u64,
    pub up: AtomicU64,
    pub down: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConnView {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub network: &'static str,
    pub process: Option<String>,
    pub rule: String,
    pub outbound: String,
    pub engine: Option<String>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub up: u64,
    pub down: u64,
    pub error: Option<String>,
}

impl ConnEntry {
    fn view(&self, ended_ms: Option<u64>, error: Option<String>) -> ConnView {
        ConnView {
            id: self.id,
            host: self.host.clone(),
            port: self.port,
            network: self.network,
            process: self.process.clone(),
            rule: self.rule.clone(),
            outbound: self.outbound.clone(),
            engine: self.engine.clone(),
            started_ms: self.started_ms,
            ended_ms,
            up: self.up.load(Relaxed),
            down: self.down.load(Relaxed),
            error,
        }
    }
}

#[derive(Default)]
pub struct Stats {
    pub up: AtomicU64,
    pub down: AtomicU64,
    next_id: AtomicU64,
    active: Mutex<HashMap<u64, Arc<ConnEntry>>>,
    closed: Mutex<VecDeque<ConnView>>,
    /// Where connection events are written (set once at start).
    logs: OnceLock<Arc<LogBuf>>,
    /// "Verbose log": every connection, not only failures.
    verbose: AtomicBool,
    throttle: Throttle,
}

/// "1.2 KB", "3.4 MB" for the log.
fn bytes(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// "tcp firefox → example.com:443 · rule geosite:ru → direct (Xray)"
fn describe(e: &ConnEntry) -> String {
    let process = e.process.as_deref().map(|p| format!("{p} ")).unwrap_or_default();
    let engine = e.engine.as_deref().map(|x| format!(" ({x})")).unwrap_or_default();
    let rule = if e.rule.is_empty() { "final".to_string() } else { e.rule.clone() };
    format!("{} {process}→ {}:{} · rule {rule} → {}{engine}", e.network, e.host, e.port, e.outbound)
}

fn secs(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

impl Stats {
    pub fn set_logs(&self, logs: Arc<LogBuf>) {
        let _ = self.logs.set(logs);
    }

    pub fn set_verbose(&self, on: bool) {
        self.verbose.store(on, Relaxed);
    }

    pub fn verbose(&self) -> bool {
        self.verbose.load(Relaxed)
    }

    pub fn open(
        &self,
        host: &str,
        port: u16,
        network: &'static str,
        process: Option<&str>,
        rule: String,
        outbound: String,
        engine: Option<String>,
    ) -> Arc<ConnEntry> {
        let e = Arc::new(ConnEntry {
            id: self.next_id.fetch_add(1, Relaxed) + 1,
            host: host.to_string(),
            port,
            network,
            process: process.map(|p| p.to_string()),
            rule,
            outbound,
            engine,
            started_ms: now_ms(),
            up: AtomicU64::new(0),
            down: AtomicU64::new(0),
        });
        self.active.lock().unwrap().insert(e.id, e.clone());
        if self.verbose() {
            if let Some(l) = self.logs.get() {
                l.debug(format!("[conn] open {}", describe(&e)));
            }
        }
        e
    }

    pub fn add(&self, conn: &ConnEntry, n: u64, upload: bool) {
        if upload {
            conn.up.fetch_add(n, Relaxed);
            self.up.fetch_add(n, Relaxed);
        } else {
            conn.down.fetch_add(n, Relaxed);
            self.down.fetch_add(n, Relaxed);
        }
    }

    pub fn close(&self, conn: &ConnEntry, error: Option<String>) {
        self.active.lock().unwrap().remove(&conn.id);
        self.log_close(conn, error.as_deref());
        let mut c = self.closed.lock().unwrap();
        if c.len() >= CLOSED_KEEP {
            c.pop_front();
        }
        c.push_back(conn.view(Some(now_ms()), error));
    }

    /// Failures that left the page empty (nothing received) are always logged, once a
    /// minute per host and error; everything else only with "verbose log" on.
    fn log_close(&self, c: &ConnEntry, error: Option<&str>) {
        let Some(l) = self.logs.get() else { return };
        let verbose = self.verbose();
        let took = secs(now_ms().saturating_sub(c.started_ms));
        let (up, down) = (c.up.load(Relaxed), c.down.load(Relaxed));
        match error {
            Some(e) if e.contains("blocked by rule") => {
                if verbose {
                    l.debug(format!("[conn] blocked {}", describe(c)));
                }
            }
            Some(e) if down == 0 => {
                if verbose || self.throttle.allow(&format!("{}:{}|{}|{e}", c.host, c.port, c.outbound), 60_000) {
                    l.warn(format!("[conn] failed {} after {took}: {e}", describe(c)));
                }
            }
            Some(e) => {
                if verbose {
                    l.debug(format!("[conn] broken {} after {took} (↑ {} ↓ {}): {e}", describe(c), bytes(up), bytes(down)));
                }
            }
            None => {
                if verbose {
                    l.debug(format!("[conn] closed {} after {took} (↑ {} ↓ {})", describe(c), bytes(up), bytes(down)));
                }
            }
        }
    }

    /// Recently closed connections that ended with an error (for the diagnostic report).
    pub fn recent_errors(&self, n: usize) -> Vec<ConnView> {
        let c = self.closed.lock().unwrap();
        let mut v: Vec<ConnView> = c.iter().rev().filter(|x| x.error.as_deref().is_some_and(|e| !e.contains("blocked by rule"))).take(n).cloned().collect();
        v.reverse();
        v
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().unwrap().len()
    }

    /// Active connections first (newest first), then recently closed ones.
    pub fn connections(&self) -> Vec<ConnView> {
        let mut active: Vec<ConnView> = self.active.lock().unwrap().values().map(|e| e.view(None, None)).collect();
        active.sort_by(|a, b| b.started_ms.cmp(&a.started_ms));
        let closed: Vec<ConnView> = self.closed.lock().unwrap().iter().rev().cloned().collect();
        active.extend(closed);
        active
    }

    pub fn clear_closed(&self) {
        self.closed.lock().unwrap().clear();
    }
}
