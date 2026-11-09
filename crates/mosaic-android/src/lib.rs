//! JNI bridge for the Android app (`io.github.loopch.mosaic.Native`).
//!
//! The Kotlin side is thin: a WebView with the same UI as the desktop app, and a
//! VpnService. Every UI command goes through `invoke(cmd, argsJson)`, which runs
//! `mosaic_core::api::dispatch` on a Tokio runtime owned by this library.
//!
//! Only `init` must be called first; all functions are safe to call from any
//! thread except the UI thread (they block until the command finishes).

#![cfg(unix)]

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::{Arc, OnceLock};

use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use jni::JNIEnv;
use serde_json::{json, Value};

use mosaic_core::Controller;

struct Core {
    rt: tokio::runtime::Runtime,
    ctl: Arc<Controller>,
}

static CORE: OnceLock<Core> = OnceLock::new();

fn text(env: &mut JNIEnv, s: &JString) -> String {
    env.get_string(s).map(|s| s.into()).unwrap_or_default()
}

fn out(env: &mut JNIEnv, s: String) -> jstring {
    env.new_string(s).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

/// `init(dataDir, nativeLibDir, assetsDir)`: where Mosaic keeps its state, where the
/// engine binaries (libxray.so, libsingbox.so) are, and where geoip/geosite live.
#[no_mangle]
pub extern "system" fn Java_io_github_loopch_mosaic_Native_init(
    mut env: JNIEnv,
    _class: JClass,
    data_dir: JString,
    lib_dir: JString,
    assets_dir: JString,
) -> jstring {
    let (data, lib, assets) = (text(&mut env, &data_dir), text(&mut env, &lib_dir), text(&mut env, &assets_dir));
    if CORE.get().is_none() {
        // Set before any thread of ours starts; engines inherit them.
        std::env::set_var("MOSAIC_HOME", &data);
        std::env::set_var("MOSAIC_ENGINES", &lib);
        std::env::set_var("MOSAIC_ASSETS", &assets);
        std::env::set_var("XRAY_LOCATION_ASSET", &assets);
        std::env::set_var("TMPDIR", format!("{data}/tmp"));
        let _ = std::fs::create_dir_all(format!("{data}/tmp"));
        let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("mosaic").build() {
            Ok(rt) => rt,
            Err(e) => return out(&mut env, format!("cannot start the runtime: {e}")),
        };
        let ctl = Arc::new(Controller::load());
        rt.spawn(ctl.clone().run_background());
        let _ = CORE.set(Core { rt, ctl });
    }
    out(&mut env, String::new())
}

/// `invoke(cmd, argsJson)` → `{"ok": value}` or `{"err": "message"}`.
#[no_mangle]
pub extern "system" fn Java_io_github_loopch_mosaic_Native_invoke(mut env: JNIEnv, _class: JClass, cmd: JString, args: JString) -> jstring {
    let (cmd, args) = (text(&mut env, &cmd), text(&mut env, &args));
    let reply = match CORE.get() {
        None => json!({ "err": "Mosaic core is not initialised" }),
        Some(core) => {
            let args: Value = serde_json::from_str(&args).unwrap_or(Value::Null);
            match core.rt.block_on(mosaic_core::api::dispatch(&core.ctl, &cmd, &args)) {
                Ok(v) => json!({ "ok": v }),
                Err(e) => json!({ "err": e }),
            }
        }
    };
    out(&mut env, reply.to_string())
}

/// `startVpn(fd)`: connect in VPN mode on the interface VpnService established.
/// Mosaic takes ownership of `fd`. Returns "" or an error message.
#[no_mangle]
pub extern "system" fn Java_io_github_loopch_mosaic_Native_startVpn(mut env: JNIEnv, _class: JClass, fd: jint) -> jstring {
    let res = match CORE.get() {
        None => Err("Mosaic core is not initialised".to_string()),
        Some(core) => core.rt.block_on(async {
            core.ctl.disconnect().await;
            core.ctl.set_tun_fd(Some(fd));
            let r = core.ctl.connect().await;
            // Not used (connecting failed before the TUN started): close it so the VPN goes away.
            if let Some(unused) = core.ctl.set_tun_fd(None) {
                // SAFETY: we own the descriptor and nothing else refers to it.
                drop(unsafe { OwnedFd::from_raw_fd(unused) });
            }
            r
        }),
    };
    out(&mut env, res.err().unwrap_or_default())
}
