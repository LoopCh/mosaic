//! Mosaic core: everything except the UI.
//!
//! * `model`   — the unified config model (IR): nodes, rules, settings
//! * `links`   — import of share links, base64 subscriptions, WireGuard .conf
//! * `engines` — compilers from IR to Xray / sing-box configs + capability matrix
//! * `process` — engine process lifecycle
//! * `hub`     — the data plane: mixed SOCKS5/HTTP inbound, router, dispatcher
//! * `udp`     — UDP ASSOCIATE inbound and per-flow UDP relays
//! * `geo`     — geoip.dat / geosite.dat reader
//! * `dns`, `resolver`, `sniff`, `net` — helpers for TUN modes
//! * `tun_native` — Mosaic's own TUN stack (feature `native-tun`, experimental)
//! * `controller` — glues everything together; this is what the UI and CLI call
//! * `api`     — the UI commands as one JSON entry point (used by the Android app)

pub mod api;
pub mod b64;
pub mod controller;
pub mod diag;
pub mod dns;
pub mod engines;
pub mod formats;
pub mod geo;
pub mod hub;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod linux_tun;
pub mod links;
pub mod model;
pub mod net;
pub mod process;
pub mod procinfo;
pub mod resolver;
pub mod rules;
pub mod sniff;
pub mod stats;
pub mod store;
pub mod sys;
#[cfg(feature = "native-tun")]
pub mod tun_native;
pub mod udp;
pub mod updates;
pub mod util;
#[cfg(windows)]
pub mod wfp;

pub use controller::Controller;
