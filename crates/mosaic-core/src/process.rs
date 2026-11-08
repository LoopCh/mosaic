//! Engine process lifecycle: spawn without a console window, pump logs,
//! wait until inbounds are listening, kill on stop/drop.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::model::EngineKind;
use crate::util::LogBuf;

pub struct EngineProcess {
    pub kind: EngineKind,
    /// "engine" for outbound engines, "tun" for the TUN adapter.
    pub role: &'static str,
    pub pid: Option<u32>,
    child: Child,
}

impl EngineProcess {
    pub fn tag(kind: EngineKind, role: &str) -> String {
        if role == "tun" {
            format!("{}/tun", kind.label())
        } else {
            kind.label().to_string()
        }
    }

    pub async fn spawn(kind: EngineKind, role: &'static str, exe: &Path, config: &Path, logs: Arc<LogBuf>) -> Result<Self, String> {
        let mut cmd = Command::new(exe);
        cmd.arg("run")
            .arg("-c")
            .arg(config)
            .current_dir(exe.parent().unwrap_or_else(|| Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flashing for engine processes.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
        let tag = Self::tag(kind, role);
        if let Some(out) = child.stdout.take() {
            pump(out, tag.clone(), logs.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, tag.clone(), logs.clone());
        }
        logs.push(format!("[hub] started {tag} (pid {})", child.id().unwrap_or(0)));
        Ok(Self { kind, role, pid: child.id(), child })
    }

    /// Some(exit status) if the process has already exited.
    pub fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(st)) => Some(st.to_string()),
            _ => None,
        }
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
    }

    /// Wait until every port accepts connections, or fail early if the process dies.
    pub async fn wait_ports(&mut self, ports: &[u16], timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        for &p in ports {
            loop {
                if let Some(st) = self.exited() {
                    return Err(format!("{} exited right after start ({st})", self.kind.label()));
                }
                if TcpStream::connect(("127.0.0.1", p)).await.is_ok() {
                    break;
                }
                if Instant::now() > deadline {
                    return Err(format!("{} did not open port {p} within {} s", self.kind.label(), timeout.as_secs()));
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
        }
        Ok(())
    }
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(r: R, tag: String, logs: Arc<LogBuf>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            logs.push(format!("[{tag}] {l}"));
        }
    });
}

/// Ask the OS for `n` distinct free loopback ports.
pub fn alloc_ports(n: usize) -> Result<Vec<u16>, String> {
    let mut hold = Vec::with_capacity(n);
    let mut ports = Vec::with_capacity(n);
    for _ in 0..n {
        let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no free port: {e}"))?;
        ports.push(l.local_addr().map_err(|e| e.to_string())?.port());
        hold.push(l);
    }
    drop(hold);
    Ok(ports)
}

/// `<engine> version`, first line, for the Engines screen.
pub async fn version(exe: &Path) -> Option<String> {
    let mut cmd = Command::new(exe);
    cmd.arg("version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }
    let out = tokio::time::timeout(Duration::from_secs(4), cmd.output()).await.ok()?.ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next().map(|s| s.trim().to_string())
}
