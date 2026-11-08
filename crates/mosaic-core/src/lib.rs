//! Mosaic core: everything except the UI.
//!
//! * `model`   — the unified config model (IR): nodes, rules, settings
//! * `links`   — import of share links, base64 subscriptions, WireGuard .conf
//! * `engines` — compilers from IR to Xray / sing-box configs + capability matrix
//! * `process` — engine process lifecycle
//! * `hub`     — the data plane: mixed SOCKS5/HTTP inbound, router, dispatcher
//! * `controller` — glues everything together; this is what the UI and CLI call

pub mod b64;
pub mod controller;
pub mod engines;
pub mod hub;
pub mod links;
pub mod model;
pub mod process;
pub mod rules;
pub mod stats;
pub mod store;
pub mod sys;
pub mod util;

pub use controller::Controller;
