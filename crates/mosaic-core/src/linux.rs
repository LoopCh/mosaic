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

const CAPS: &str = "cap_net_admin,cap_net_raw,cap_net_bind_service+ep";

/// True if `exe` carries file capabilities (set by `grant_tun`).
pub fn has_caps(exe: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(exe.as_os_str().as_bytes()) else { return false };
    let name = b"security.capability\0";
    let n = unsafe { libc::getxattr(path.as_ptr(), name.as_ptr() as *const libc::c_char, std::ptr::null_mut(), 0) };
    n > 0
}

/// Let `exe` (our copy of sing-box) create TUN interfaces without running Mosaic as
/// root: one password prompt (polkit) that runs `setcap` on that file.
pub fn grant_tun(exe: &Path) -> Result<(), String> {
    if has_caps(exe) {
        return Ok(());
    }
    let setcap = which("setcap").ok_or("setcap not found: install libcap2-bin (Debian/Ubuntu) or libcap (Fedora/Arch)")?;
    let pkexec = which("pkexec").ok_or("pkexec not found: install polkit, or run once: sudo setcap cap_net_admin,cap_net_raw,cap_net_bind_service+ep <sing-box>")?;
    let out = Command::new(pkexec)
        .arg(&setcap)
        .arg(CAPS)
        .arg(exe)
        .output()
        .map_err(|e| format!("pkexec: {e}"))?;
    if out.status.success() && has_caps(exe) {
        Ok(())
    } else if out.status.code() == Some(126) || out.status.code() == Some(127) {
        Err("permission for TUN was not granted".into())
    } else {
        Err(format!("setcap failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}
