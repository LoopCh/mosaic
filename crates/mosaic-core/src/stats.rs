//! Traffic counters and the connection journal ("why did this go there?").

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::util::now_ms;

const CLOSED_KEEP: usize = 300;

pub struct ConnEntry {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub network: &'static str,
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
}

impl Stats {
    pub fn open(&self, host: &str, port: u16, network: &'static str, rule: String, outbound: String, engine: Option<String>) -> Arc<ConnEntry> {
        let e = Arc::new(ConnEntry {
            id: self.next_id.fetch_add(1, Relaxed) + 1,
            host: host.to_string(),
            port,
            network,
            rule,
            outbound,
            engine,
            started_ms: now_ms(),
            up: AtomicU64::new(0),
            down: AtomicU64::new(0),
        });
        self.active.lock().unwrap().insert(e.id, e.clone());
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
        let mut c = self.closed.lock().unwrap();
        if c.len() >= CLOSED_KEEP {
            c.pop_front();
        }
        c.push_back(conn.view(Some(now_ms()), error));
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
