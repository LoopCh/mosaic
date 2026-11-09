//! OS integration: system proxy and elevation check.

/// Returns Ok(true) if the system proxy was set, Ok(false) if this OS is not
/// supported yet (the hub still listens and can be configured manually).
pub fn set_system_proxy(port: u16) -> Result<bool, String> {
    #[cfg(windows)]
    {
        win::set(port).map(|_| true)
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        Ok(false)
    }
}

pub fn unset_system_proxy() -> Result<(), String> {
    #[cfg(windows)]
    {
        win::unset()
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

/// Start Mosaic with Windows. When running elevated, a scheduled task with
/// highest privileges is used (so TUN works at logon); otherwise the
/// per-user Run key. `enable = false` removes both.
pub fn set_autostart(enable: bool, exe: &std::path::Path, elevated: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::set_autostart(enable, exe, elevated)
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, elevated);
        if enable {
            Err("autostart is implemented for Windows only so far".into())
        } else {
            Ok(())
        }
    }
}

/// True if the OS user interface language is Russian (for the tray menu).
pub fn ui_lang_is_ru() -> bool {
    #[cfg(windows)]
    {
        win::ui_lang_is_ru()
    }
    #[cfg(not(windows))]
    {
        std::env::var("LANG").map(|l| l.starts_with("ru")).unwrap_or(false)
    }
}

pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        win::is_admin()
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
            .unwrap_or(false)
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    const BYPASS: &str = "localhost;127.*;10.*;192.168.*;172.16.*;172.17.*;172.18.*;172.19.*;172.2*;172.30.*;172.31.*;<local>";
    const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
    const INTERNET_OPTION_REFRESH: u32 = 37;

    #[link(name = "wininet")]
    extern "system" {
        fn InternetSetOptionW(h: *mut c_void, option: u32, buffer: *mut c_void, len: u32) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn IsUserAnAdmin() -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const TASK: &str = "Mosaic";

    fn run_quiet(cmd: &str, args: &[&str]) -> Result<(), String> {
        let out = Command::new(cmd)
            .args(args)
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("{cmd}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            Err(format!("{cmd}: {}", text.trim()))
        }
    }

    pub fn set_autostart(enable: bool, exe: &std::path::Path, elevated: bool) -> Result<(), String> {
        let _ = run_quiet("reg", &["delete", RUN_KEY, "/v", TASK, "/f"]);
        let removed_task = run_quiet("schtasks", &["/Delete", "/TN", TASK, "/F"]);
        if !enable {
            if let Err(e) = removed_task {
                // Only a problem if the task exists and we lack the rights to delete it.
                if run_quiet("schtasks", &["/Query", "/TN", TASK]).is_ok() {
                    return Err(format!("run Mosaic as administrator to remove the elevated autostart task ({e})"));
                }
            }
            return Ok(());
        }
        let command = format!("\"{}\" --minimized", exe.display());
        if elevated {
            let user = format!(
                "{}\\{}",
                std::env::var("USERDOMAIN").unwrap_or_default(),
                std::env::var("USERNAME").unwrap_or_default()
            );
            run_quiet(
                "schtasks",
                &["/Create", "/F", "/TN", TASK, "/SC", "ONLOGON", "/RL", "HIGHEST", "/IT", "/RU", &user, "/DELAY", "0000:10", "/TR", &command],
            )
        } else {
            run_quiet("reg", &["add", RUN_KEY, "/v", TASK, "/t", "REG_SZ", "/d", &command, "/f"])
        }
    }

    pub fn ui_lang_is_ru() -> bool {
        (unsafe { GetUserDefaultUILanguage() } & 0x3ff) == 0x19
    }

    fn reg(args: &[&str]) -> Result<(), String> {
        let out = Command::new("reg")
            .args(args)
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("reg.exe: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("reg.exe: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    fn refresh() {
        unsafe {
            InternetSetOptionW(std::ptr::null_mut(), INTERNET_OPTION_SETTINGS_CHANGED, std::ptr::null_mut(), 0);
            InternetSetOptionW(std::ptr::null_mut(), INTERNET_OPTION_REFRESH, std::ptr::null_mut(), 0);
        }
    }

    pub fn set(port: u16) -> Result<(), String> {
        let server = format!("127.0.0.1:{port}");
        reg(&["add", KEY, "/v", "ProxyServer", "/t", "REG_SZ", "/d", &server, "/f"])?;
        reg(&["add", KEY, "/v", "ProxyOverride", "/t", "REG_SZ", "/d", BYPASS, "/f"])?;
        reg(&["add", KEY, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "1", "/f"])?;
        refresh();
        Ok(())
    }

    pub fn unset() -> Result<(), String> {
        reg(&["add", KEY, "/v", "ProxyEnable", "/t", "REG_DWORD", "/d", "0", "/f"])?;
        refresh();
        Ok(())
    }

    pub fn is_admin() -> bool {
        unsafe { IsUserAnAdmin() != 0 }
    }
}
