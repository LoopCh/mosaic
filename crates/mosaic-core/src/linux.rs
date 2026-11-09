//! Linux desktop integration: system proxy (GNOME / KDE), autostart (.desktop),
//! engine binaries in the user's data folder, and the TUN permission for sing-box.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .chain(["/usr/sbin", "/sbin", "/usr/bin", "/bin"].iter().map(PathBuf::from))
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

// ---------------------------------------------------------------- system proxy

const IGNORE: &str = "['localhost', '127.0.0.0/8', '::1', '10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '*.local']";

/// GNOME (and desktops using its settings: Cinnamon, Budgie, Pantheon…) and KDE Plasma.
/// Ok(true) if at least one desktop accepted the setting.
pub fn set_system_proxy(port: u16) -> Result<bool, String> {
    let p = port.to_string();
    let mut ok = false;
    if which("gsettings").is_some() {
        let g = |args: &[&str]| run("gsettings", args);
        let mut res = g(&["set", "org.gnome.system.proxy", "mode", "manual"]);
        for scheme in ["http", "https", "socks"] {
            let schema = format!("org.gnome.system.proxy.{scheme}");
            res = res.and(g(&["set", &schema, "host", "127.0.0.1"])).and(g(&["set", &schema, "port", &p]));
        }
        res = res.and(g(&["set", "org.gnome.system.proxy", "ignore-hosts", IGNORE]));
        ok |= res.is_ok();
    }
    for kw in ["kwriteconfig6", "kwriteconfig5"] {
        if which(kw).is_none() {
            continue;
        }
        let k = |key: &str, val: &str| run(kw, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", key, val]);
        let url = format!("http://127.0.0.1 {port}");
        let socks = format!("socks://127.0.0.1 {port}");
        let res = k("ProxyType", "1")
            .and(k("httpProxy", &url))
            .and(k("httpsProxy", &url))
            .and(k("socksProxy", &socks))
            .and(k("NoProxyFor", "localhost,127.0.0.1,::1,.local"));
        if res.is_ok() {
            kde_notify();
            ok = true;
        }
        break;
    }
    Ok(ok)
}

pub fn unset_system_proxy() -> Result<(), String> {
    if which("gsettings").is_some() {
        let _ = run("gsettings", &["set", "org.gnome.system.proxy", "mode", "none"]);
    }
    for kw in ["kwriteconfig6", "kwriteconfig5"] {
        if which(kw).is_some() {
            let _ = run(kw, &["--file", "kioslaverc", "--group", "Proxy Settings", "--key", "ProxyType", "0"]);
            kde_notify();
            break;
        }
    }
    Ok(())
}

/// Tell running KDE apps that the proxy settings changed.
fn kde_notify() {
    let _ = Command::new("dbus-send")
        .args(["--type=signal", "/KIO/Scheduler", "org.kde.KIO.Scheduler.reparseSlaveConfiguration", "string:"])
        .output();
}

// ---------------------------------------------------------------- autostart

fn autostart_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("mosaic.desktop"))
}

/// The command to start this Mosaic: the AppImage file itself when running from one.
pub fn launcher_path(exe: &Path) -> PathBuf {
    std::env::var_os("APPIMAGE").map(PathBuf::from).unwrap_or_else(|| exe.to_path_buf())
}

pub fn set_autostart(enable: bool, exe: &Path) -> Result<(), String> {
    let file = autostart_file().ok_or("no home folder")?;
    if !enable {
        let _ = std::fs::remove_file(&file);
        return Ok(());
    }
    std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
    let exec = launcher_path(exe).display().to_string().replace('"', "\\\"");
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Mosaic\nComment=Multi-engine proxy and VPN client\nExec=\"{exec}\" --minimized\nIcon=mosaic\nTerminal=false\nX-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=5\n"
    );
    std::fs::write(&file, text).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- engines

/// Where the installer put the engines: deb/rpm (/usr/lib/Mosaic/engines), AppImage
/// ($APPDIR/usr/lib/Mosaic/engines), or next to the binary (portable / development).
pub fn bundled_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(appdir) = std::env::var_os("APPDIR") {
        let a = PathBuf::from(appdir);
        v.push(a.join("usr/lib/Mosaic/engines"));
        v.push(a.join("usr/lib/mosaic/engines"));
    }
    v.push(PathBuf::from("/usr/lib/Mosaic/engines"));
    v.push(PathBuf::from("/usr/lib/mosaic/engines"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("engines"));
            v.push(dir.join("../lib/Mosaic/engines"));
        }
    }
    v
}

/// Copy the bundled engines into `dest` (the user's data folder) and make them
/// executable. Installed files can be read-only (AppImage) or lose the executable
/// bit, and the TUN permission (a file capability) can only be set on our own copy.
/// Files are copied again only when they differ in size or are newer.
pub fn install_bundled(dest: &Path) -> Vec<String> {
    let mut log = Vec::new();
    let Some(src) = bundled_dirs().into_iter().find(|d| d.join("sing-box").is_file() || d.join("xray").is_file()) else {
        return log;
    };
    if std::fs::create_dir_all(dest).is_err() {
        return log;
    }
    for name in ["xray", "sing-box", "geoip.dat", "geosite.dat"] {
        let (from, to) = (src.join(name), dest.join(name));
        let Ok(m) = std::fs::metadata(&from) else { continue };
        let same = std::fs::metadata(&to)
            .map(|t| t.len() == m.len() && t.modified().ok() >= m.modified().ok())
            .unwrap_or(false);
        // 0.7.0 put the TUN capability on the engine copy itself: replace it with a clean file.
        let same = same && !(name == "sing-box" && has_caps(&to));
        if same {
            continue;
        }
        let tmp = dest.join(format!(".{name}.tmp"));
        if std::fs::copy(&from, &tmp).is_ok() {
            if !name.ends_with(".dat") {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
            }
            if std::fs::rename(&tmp, &to).is_ok() {
                log.push(format!("[hub] installed bundled {name}"));
            }
        }
    }
    log
}

// ---------------------------------------------------------------- TUN permission

/// A separate copy of sing-box only for the TUN adapter: the capability goes to this
/// file alone. (A process started from a file with capabilities becomes "non-dumpable",
/// so the adapter could no longer see which program owns its sockets — that broke
/// loop prevention for the sing-box engine when both shared one file.)
/// Copy `src` to `dst` (executable) unless an identical-looking copy is there already.
/// A fresh copy has no capabilities: they are granted again on the next TUN connect.
pub fn copy_if_changed(src: &Path, dst: &Path) -> Result<PathBuf, String> {
    let src_meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let same = std::fs::metadata(dst).map(|d| d.len() == src_meta.len() && d.modified().ok() >= src_meta.modified().ok()).unwrap_or(false);
    if !same {
        let dir = dst.parent().ok_or("bad path")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!(".{}.tmp", dst.file_name().unwrap_or_default().to_string_lossy()));
        std::fs::copy(src, &tmp).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        std::fs::rename(&tmp, dst).map_err(|e| e.to_string())?;
    }
    Ok(dst.to_path_buf())
}

pub fn tun_binary(engine: &Path, dir: &Path) -> Result<PathBuf, String> {
    let dst = dir.join("sing-box-tun");
    let src_meta = std::fs::metadata(engine).map_err(|e| format!("{}: {e}", engine.display()))?;
    let same = std::fs::metadata(&dst).map(|d| d.len() == src_meta.len() && d.modified().ok() >= src_meta.modified().ok()).unwrap_or(false);
    if !same {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(".sing-box-tun.tmp");
        std::fs::copy(engine, &tmp).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    }
    Ok(dst)
}

const CAPS: &str = "cap_net_admin,cap_net_raw,cap_net_bind_service+ep";

/// True if `exe` carries file capabilities (set by `grant_tun`).
pub fn has_caps(exe: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(exe.as_os_str().as_bytes()) else { return false };
    let name = b"security.capability\0";
    let n = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr() as *const libc::c_char, std::ptr::null_mut(), 0) };
    n > 0
}

const POLKIT_RULE: &str = "/etc/polkit-1/rules.d/49-mosaic.rules";

/// systemd-resolved is in use: the TUN adapter then sets the DNS of its interface through
/// it, and polkit asks for a password every time ("reset name resolution settings",
/// "flush DNS caches") unless a rule allows it.
fn resolved_active() -> bool {
    Path::new("/run/systemd/resolve/io.systemd.Resolve").exists() || Path::new("/run/systemd/resolve/resolv.conf").exists()
}

fn polkit_rule_text(user: &str) -> String {
    let user = user.replace(['"', '\\'], "");
    format!(
        r#"// Installed by Mosaic: lets {user} set and reset the DNS of network interfaces
// (what the Mosaic TUN adapter does on connect/disconnect) without a password.
// Delete this file to undo.
polkit.addRule(function(action, subject) {{
    var ids = ["org.freedesktop.resolve1.set-dns-servers", "org.freedesktop.resolve1.set-domains",
               "org.freedesktop.resolve1.set-default-route", "org.freedesktop.resolve1.set-llmnr",
               "org.freedesktop.resolve1.set-mdns", "org.freedesktop.resolve1.set-dnssec",
               "org.freedesktop.resolve1.set-dns-over-tls", "org.freedesktop.resolve1.revert",
               "org.freedesktop.resolve1.flush-caches", "org.freedesktop.resolve1.reset-server-features"];
    if (ids.indexOf(action.id) >= 0 && subject.user == "{user}" && subject.local && subject.active) {{
        return polkit.Result.YES;
    }}
}});
"#
    )
}

/// Nothing to ask: the capability is set and (with systemd-resolved) the polkit rule exists.
pub fn tun_ready(exe: &Path) -> bool {
    let user = std::env::var("USER").unwrap_or_default();
    has_caps(exe) && !(resolved_active() && !user.is_empty() && user != "root" && !Path::new(POLKIT_RULE).exists())
}

/// Let `exe` (our copy of sing-box) create TUN interfaces without running Mosaic as
/// root, and (with systemd-resolved) let the user change interface DNS without a password.
/// One password prompt (polkit) for both; nothing happens if both are already in place.
pub fn grant_tun(exe: &Path) -> Result<(), String> {
    let need_caps = !has_caps(exe);
    let user = std::env::var("USER").unwrap_or_default();
    let need_rule = resolved_active() && !user.is_empty() && user != "root" && !Path::new(POLKIT_RULE).exists();
    if !need_caps && !need_rule {
        return Ok(());
    }
    let pkexec = which("pkexec").ok_or("pkexec not found: install polkit, or run once: sudo setcap cap_net_admin,cap_net_raw,cap_net_bind_service+ep <sing-box>")?;
    let setcap = which("setcap");
    if need_caps && setcap.is_none() {
        return Err("setcap not found: install libcap2-bin (Debian/Ubuntu) or libcap (Fedora/Arch)".into());
    }
    let rule_tmp = std::env::temp_dir().join(format!("mosaic-polkit-{}.rules", std::process::id()));
    if need_rule {
        std::fs::write(&rule_tmp, polkit_rule_text(&user)).map_err(|e| e.to_string())?;
    }
    // One privileged shell: $1 setcap, $2 caps, $3 exe, $4 rule file ("" = skip), $5 target.
    let script = r#"set -e
if [ -n "$1" ]; then "$1" "$2" "$3"; fi
if [ -n "$4" ]; then install -D -m 0644 "$4" "$5"; fi"#;
    let out = Command::new(pkexec)
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(if need_caps { setcap.unwrap().into_os_string() } else { "".into() })
        .arg(CAPS)
        .arg(exe)
        .arg(if need_rule { rule_tmp.clone().into_os_string() } else { "".into() })
        .arg(POLKIT_RULE)
        .output()
        .map_err(|e| format!("pkexec: {e}"));
    let _ = std::fs::remove_file(&rule_tmp);
    let out = out?;
    if out.status.code() == Some(126) || out.status.code() == Some(127) {
        return Err("permission for TUN was not granted".into());
    }
    if need_caps && !has_caps(exe) {
        return Err(format!("setcap failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}
