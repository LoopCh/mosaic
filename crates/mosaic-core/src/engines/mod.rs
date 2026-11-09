//! Engines: capability matrix + config compilers + where to find binaries.
//!
//! Every engine is used the same way by the hub: it gets a list of *slots*
//! (node + local port) and must expose one SOCKS5 inbound per slot on
//! 127.0.0.1:<port> that forwards everything to that node.

use std::path::PathBuf;

use serde_json::Value;

use crate::model::{EngineKind, Node};

pub mod singbox;
pub mod xray;

pub type Slot = (Node, u16);

pub fn compile(kind: EngineKind, slots: &[Slot]) -> Result<Value, String> {
    match kind {
        EngineKind::Xray => xray::compile(slots),
        EngineKind::Singbox => singbox::compile(slots),
    }
}

/// Directories searched for engine binaries, in order. Engines updated from
/// the app live in the data folder and take priority over the bundled ones.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("MOSAIC_ENGINES") {
        v.push(PathBuf::from(d));
    }
    v.push(updated_dir());
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("engines"));
            v.push(dir.to_path_buf());
            // target/release/mosaic.exe -> <repo>/engines
            if let Some(p) = dir.parent() {
                v.push(p.join("engines"));
                if let Some(pp) = p.parent() {
                    v.push(pp.join("engines"));
                }
            }
        }
    }
    if let Ok(c) = std::env::current_dir() {
        v.push(c.join("engines"));
    }
    v
}

/// Where engine updates are installed (%APPDATA%\Mosaic\engines on Windows).
pub fn updated_dir() -> PathBuf {
    crate::store::data_dir().join("engines")
}

pub fn find(kind: EngineKind) -> Option<PathBuf> {
    search_dirs().into_iter().map(|d| d.join(kind.exe_name())).find(|p| p.is_file())
}

pub(crate) fn sni_or<'a>(sni: &'a str, server: &'a str) -> &'a str {
    if sni.is_empty() {
        server
    } else {
        sni
    }
}

pub(crate) fn or_default<'a>(v: &'a str, d: &'a str) -> &'a str {
    if v.is_empty() {
        d
    } else {
        v
    }
}
