//! mosaic-tun-helper: creates and configures the TUN interface for Mosaic's own
//! TUN stack on Linux. Mosaic runs it from a copy that carries `cap_net_admin`
//! (one password prompt, like the sing-box adapter), so Mosaic itself never runs
//! as root.
//!
//!   1. creates a *persistent* TUN interface owned by the user, so the unprivileged
//!      Mosaic process can attach to it (TUNSETIFF on an interface you own needs no rights);
//!   2. after Mosaic has attached ("go" on stdin), sets the routes: 0/1 + 128/1 into the
//!      TUN, /32 bypass routes for node servers via the physical gateway, the system's DNS
//!      servers into the TUN, and the interface DNS in systemd-resolved;
//!   3. waits for Mosaic's word on stdin and then:
//!      * `stop` — removes everything (Mosaic disconnected or quit);
//!      * `hold` — exits and leaves the interface and its routes (Mosaic reconnects; the
//!        next helper takes the interface over, so traffic never leaves the tunnel);
//!      * stdin closed without a word (Mosaic crashed): with the kill switch on, the
//!        interface stays and swallows all traffic — nothing leaks; otherwise it is removed.
//!
//!   `clean` removes a left-over interface (the user lifts the kill switch).
//!
//! Protocol on stdout: `created`, then `ready`, or `error: …`; `warn: …` lines are logged.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("mosaic-tun-helper is only used on Linux");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    imp::main();
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io::{BufRead, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const TUN_NAME: &str = "Mosaic";

    const TUNSETIFF: libc::c_ulong = 0x4004_54ca;
    const TUNSETPERSIST: libc::c_ulong = 0x4004_54cb;
    const TUNSETOWNER: libc::c_ulong = 0x4004_54cc;
    const IFF_TUN: libc::c_short = 0x0001;
    const IFF_NO_PI: libc::c_short = 0x1000;

    #[repr(C)]
    struct IfReq {
        name: [u8; 16],
        flags: libc::c_short,
        _pad: [u8; 22],
    }

    fn open_tun(name: &str) -> std::io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let mut req = IfReq { name: [0; 16], flags: IFF_TUN | IFF_NO_PI, _pad: [0; 22] };
        let n = name.len().min(15);
        req.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        if unsafe { libc::ioctl(fd, TUNSETIFF as _, &mut req as *mut IfReq) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(owned)
    }


    const CAP_NET_ADMIN: u32 = 12;
    const CAP_NET_RAW: u32 = 13;
    const PR_CAP_AMBIENT: libc::c_int = 47;
    const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;

    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: libc::c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }

    /// Make CAP_NET_ADMIN/RAW "ambient" so the `ip` and `resolvectl` we run inherit them.
    fn raise_ambient() -> Result<(), String> {
        let mut hdr = CapHeader { version: 0x2008_0522, pid: 0 };
        let mut data = [CapData::default(); 2];
        if unsafe { libc::syscall(libc::SYS_capget, &mut hdr as *mut CapHeader, data.as_mut_ptr()) } != 0 {
            return Err(format!("capget: {}", std::io::Error::last_os_error()));
        }
        let want = (1u32 << CAP_NET_ADMIN) | (1u32 << CAP_NET_RAW);
        if data[0].permitted & (1 << CAP_NET_ADMIN) == 0 {
            return Err("the helper has no CAP_NET_ADMIN (the permission was not granted)".into());
        }
        data[0].inheritable |= want & data[0].permitted;
        if unsafe { libc::syscall(libc::SYS_capset, &mut hdr as *mut CapHeader, data.as_ptr()) } != 0 {
            return Err(format!("capset: {}", std::io::Error::last_os_error()));
        }
        for cap in [CAP_NET_ADMIN, CAP_NET_RAW] {
            if data[0].permitted & (1 << cap) != 0 {
                unsafe {
                    libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, cap as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong);
                }
            }
        }
        Ok(())
    }

    fn create_persistent(name: &str, uid: u32) -> Result<(), String> {
        use std::os::fd::AsRawFd;
        let fd = open_tun(name).map_err(|e| format!("create {name}: {e}"))?;
        let raw = fd.as_raw_fd();
        if unsafe { libc::ioctl(raw, TUNSETOWNER as _, uid as libc::c_ulong) } < 0 {
            return Err(format!("TUNSETOWNER: {}", std::io::Error::last_os_error()));
        }
        if unsafe { libc::ioctl(raw, TUNSETPERSIST as _, 1 as libc::c_ulong) } < 0 {
            return Err(format!("TUNSETPERSIST: {}", std::io::Error::last_os_error()));
        }
        Ok(()) // the fd closes; the interface stays (persistent) until `ip link del`
    }

    fn which(name: &str) -> Option<PathBuf> {
        ["/usr/sbin", "/sbin", "/usr/bin", "/bin"].iter().map(|d| Path::new(d).join(name)).find(|p| p.is_file())
    }

    struct Ip(PathBuf);

    impl Ip {
        fn run(&self, args: &[&str]) -> Result<(), String> {
            let out = Command::new(&self.0).args(args).output().map_err(|e| format!("ip: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!("ip {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
            }
        }
    }

    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    /// Arguments: run <uid> <physical-if> <gateway or -> <bypass ips, comma> <dns ips, comma> <ipv6 0|1> [ks]
    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("clean") {
            std::process::exit(match clean() {
                Ok(()) => 0,
                Err(e) => {
                    say(&format!("error: {e}"));
                    1
                }
            });
        }
        if args.first().map(String::as_str) != Some("run") {
            eprintln!("mosaic-tun-helper is started by Mosaic; it is not meant to be run by hand");
            std::process::exit(2);
        }
        std::process::exit(helper_main(&args[1..]));
    }

    fn helper_main(args: &[String]) -> i32 {
        match helper(args) {
            Ok(()) => 0,
            Err(e) => {
                say(&format!("error: {e}"));
                1
            }
        }
    }

    /// Lift the kill switch: remove the interface a crashed or held run left behind.
    fn clean() -> Result<(), String> {
        raise_ambient()?;
        let ip = Ip(which("ip").ok_or("the `ip` command (iproute2) was not found")?);
        if Path::new("/sys/class/net").join(TUN_NAME).exists() {
            ip.run(&["link", "del", TUN_NAME])?;
        }
        say("clean");
        Ok(())
    }

    fn split_list(s: &str) -> Vec<String> {
        s.split(',').map(str::trim).filter(|x| !x.is_empty() && *x != "-").map(String::from).collect()
    }

    fn helper(args: &[String]) -> Result<(), String> {
        if args.len() < 6 {
            return Err("usage: --tun-helper <uid> <if> <gw|-> <bypass> <dns> <ipv6>".into());
        }
        let uid: u32 = args[0].parse().map_err(|_| "bad uid")?;
        let (phys, gw) = (args[1].clone(), args[2].clone());
        let bypass = split_list(&args[3]);
        let dns_routes = split_list(&args[4]);
        let ipv6 = args[5] == "1";
        let kill_switch = args.get(6).map(String::as_str) == Some("ks");

        raise_ambient()?;
        let ip = Ip(which("ip").ok_or("the `ip` command (iproute2) was not found")?);

        // An interface left by the previous run (a reconnect or the kill switch after a crash):
        // take it over as it is, so traffic keeps going into it and never leaks. If that is not
        // possible, start from scratch.
        let exists = Path::new("/sys/class/net").join(TUN_NAME).exists();
        if !(exists && create_persistent(TUN_NAME, uid).is_ok()) {
            let _ = ip.run(&["link", "del", TUN_NAME]);
            create_persistent(TUN_NAME, uid)?;
        }
        let mut added: Vec<Vec<String>> = Vec::new();
        let mut keep = false;
        let res = (|| -> Result<(), String> {
            let _ = ip.run(&["addr", "flush", "dev", TUN_NAME]);
            ip.run(&["addr", "add", "172.19.0.1/30", "dev", TUN_NAME])?;
            ip.run(&["link", "set", TUN_NAME, "mtu", "1500", "up"])?;
            if ipv6 {
                let _ = ip.run(&["-6", "addr", "add", "fdfe:dcba:9876::1/126", "dev", TUN_NAME]);
            }
            say("created");
            // Wait for Mosaic to attach to the interface.
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
            if line.trim() != "go" {
                return Err("Mosaic did not attach to the interface".into());
            }
            for b in &bypass {
                let dst = format!("{b}/32");
                let mut r = vec!["route".to_string(), "replace".into(), dst.clone()];
                if gw != "-" {
                    r.extend(["via".into(), gw.clone()]);
                }
                r.extend(["dev".into(), phys.clone()]);
                let args: Vec<&str> = r.iter().map(String::as_str).collect();
                match ip.run(&args) {
                    Ok(()) => added.push(vec!["route".into(), "del".into(), dst, "dev".into(), phys.clone()]),
                    Err(e) => say(&format!("warn: {e}")),
                }
            }
            // DNS servers in the local network would otherwise be reached directly (on-link route).
            for d in &dns_routes {
                let dst = format!("{d}/32");
                if let Err(e) = ip.run(&["route", "replace", &dst, "dev", TUN_NAME]) {
                    say(&format!("warn: {e}"));
                }
            }
            for half in ["0.0.0.0/1", "128.0.0.0/1"] {
                ip.run(&["route", "replace", half, "dev", TUN_NAME])?;
            }
            if ipv6 {
                for half in ["::/1", "8000::/1"] {
                    if let Err(e) = ip.run(&["-6", "route", "replace", half, "dev", TUN_NAME]) {
                        say(&format!("warn: {e}"));
                    }
                }
            }
            // systemd-resolved: send all names to the tunnel (the "~." routing domain makes this
            // link the only one used). With CAP_NET_ADMIN no password is asked.
            if let Some(rc) = which("resolvectl") {
                let ok = Command::new(&rc).args(["dns", TUN_NAME, "172.19.0.2"]).status().map(|s| s.success()).unwrap_or(false);
                if ok {
                    let _ = Command::new(&rc).args(["domain", TUN_NAME, "~."]).status();
                    let _ = Command::new(&rc).args(["default-route", TUN_NAME, "true"]).status();
                    let _ = Command::new(&rc).args(["flush-caches"]).status();
                }
            }
            if kill_switch {
                say("kill switch on: if Mosaic stops unexpectedly, traffic stays blocked");
            }
            say("ready");
            // Until Mosaic says what to do, or its end of stdin closes (it crashed).
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::stdin().lock().read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        keep = kill_switch;
                        break;
                    }
                    Ok(_) => match line.trim() {
                        "stop" => break,
                        "hold" => {
                            keep = true;
                            break;
                        }
                        _ => {}
                    },
                }
            }
            Ok(())
        })();
        if keep && res.is_ok() {
            // The interface and its routes stay: all traffic goes into a tunnel nobody reads.
            return Ok(());
        }
        for r in &added {
            let args: Vec<&str> = r.iter().map(String::as_str).collect();
            let _ = ip.run(&args);
        }
        // Routes on the interface and its resolved settings go away with it.
        let _ = ip.run(&["link", "del", TUN_NAME]);
        res
    }
}
