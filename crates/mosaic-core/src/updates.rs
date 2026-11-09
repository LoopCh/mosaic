//! Updates: engines (Xray, sing-box + geo files) and Mosaic itself, from
//! GitHub releases. Engine updates are staged and verified before they
//! replace anything; the controller rolls back if the new engine fails.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::EngineKind;

/// GitHub repository with Mosaic releases (override: MOSAIC_UPDATE_REPO=owner/name).
pub const APP_REPO: &str = "LoopCh/mosaic";
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

// ---------------------------------------------------------------- versions

/// First dotted number in a string: "Xray 25.10.15 (…)" → [25, 10, 15], "v1.12.3" → [1, 12, 3].
pub fn parse_version(s: &str) -> Option<Vec<u32>> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let run: String = s[start..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let v: Vec<u32> = run.split('.').filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    (!v.is_empty()).then_some(v)
}

/// True if `latest` is newer than `current` (missing parts count as 0).
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a > b
        }
        (Some(_), None) => true,
        _ => false,
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- GitHub

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// "sha256:<hex>" when GitHub provides it.
    pub digest: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub html_url: String,
    pub notes: String,
    pub assets: Vec<Asset>,
}

fn user_agent() -> String {
    format!("Mosaic/{} (+https://github.com/{APP_REPO})", env!("CARGO_PKG_VERSION"))
}

fn get_blocking(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url)
        .set("User-Agent", &user_agent())
        .set("Accept", "application/vnd.github+json, application/octet-stream, */*")
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    resp.into_reader().take(limit + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    if buf.len() as u64 > limit {
        return Err("download is too large".into());
    }
    Ok(buf)
}

pub async fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || get_blocking(&url, limit)).await.map_err(|e| e.to_string())?
}

pub fn parse_release(v: &Value) -> Option<Release> {
    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let assets = v
        .get("assets")?
        .as_array()?
        .iter()
        .map(|a| Asset { name: s(a, "name"), url: s(a, "browser_download_url"), digest: a.get("digest").and_then(|d| d.as_str()).map(|d| d.to_string()) })
        .collect();
    Some(Release { tag: s(v, "tag_name"), html_url: s(v, "html_url"), notes: s(v, "body"), assets })
}

pub async fn latest_release(repo: &str) -> Result<Release, String> {
    let body = download(&format!("https://api.github.com/repos/{repo}/releases/latest"), 4 * 1024 * 1024).await?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("GitHub answer: {e}"))?;
    parse_release(&v).ok_or_else(|| "GitHub answer has no release data".to_string())
}

/// Expected SHA-256 of `asset`: GitHub's digest field, else a checksum file in the release.
async fn expected_sha256(rel: &Release, asset: &Asset) -> Option<String> {
    if let Some(d) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        return Some(d.to_ascii_lowercase());
    }
    // Xray: <asset>.dgst with a "SHA2-256= <hex>" line. Mosaic: SHA256SUMS ("<hex>  <name>").
    for (name, is_sums) in [(format!("{}.dgst", asset.name), false), ("SHA256SUMS".to_string(), true)] {
        let Some(sum) = rel.assets.iter().find(|a| a.name == name) else { continue };
        let Ok(text) = download(&sum.url, 1024 * 1024).await else { continue };
        let text = String::from_utf8_lossy(&text);
        for line in text.lines() {
            if is_sums {
                let mut parts = line.split_whitespace();
                if let (Some(hex), Some(file)) = (parts.next(), parts.next()) {
                    if file.trim_start_matches('*') == asset.name {
                        return Some(hex.to_ascii_lowercase());
                    }
                }
            } else if let Some((k, v)) = line.split_once('=') {
                if k.trim().eq_ignore_ascii_case("SHA2-256") || k.trim().eq_ignore_ascii_case("SHA256") {
                    return Some(v.trim().to_ascii_lowercase());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------- engines

#[derive(Clone, Debug, Serialize)]
pub struct EngineUpdate {
    pub kind: EngineKind,
    pub current: Option<String>,
    pub latest: Option<String>,
    pub available: bool,
    pub error: Option<String>,
}

fn engine_repo(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Xray => "XTLS/Xray-core",
        EngineKind::Singbox => "SagerNet/sing-box",
    }
}

/// Release asset names for this OS / CPU: (Xray archive, sing-box archive suffix).
fn platform_assets() -> (&'static str, &'static str) {
    match (cfg!(windows), cfg!(target_arch = "aarch64")) {
        (true, false) => ("Xray-windows-64.zip", "-windows-amd64.zip"),
        (true, true) => ("Xray-windows-arm64-v8a.zip", "-windows-arm64.zip"),
        (false, false) => ("Xray-linux-64.zip", "-linux-amd64.tar.gz"),
        (false, true) => ("Xray-linux-arm64-v8a.zip", "-linux-arm64.tar.gz"),
    }
}

/// The engine archive for this OS in a release.
fn engine_asset(kind: EngineKind, rel: &Release) -> Option<Asset> {
    let (xray, sb_suffix) = platform_assets();
    rel.assets
        .iter()
        .find(|a| match kind {
            EngineKind::Xray => a.name == xray,
            EngineKind::Singbox => a
                .name
                .strip_prefix("sing-box-")
                .and_then(|r| r.strip_suffix(sb_suffix))
                .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')),
        })
        .cloned()
}

/// Files taken from the engine archive (the executable first).
fn wanted_files(kind: EngineKind) -> Vec<&'static str> {
    match kind {
        EngineKind::Xray => vec![kind.exe_name(), "geoip.dat", "geosite.dat"],
        // Newer sing-box builds ship libcronet next to the binary (naive outbound).
        EngineKind::Singbox => vec![kind.exe_name(), "libcronet.dll", "libcronet.so"],
    }
}

pub async fn engine_latest(kind: EngineKind) -> Result<Release, String> {
    latest_release(engine_repo(kind)).await
}

/// Download, verify and unpack the latest engine into `staging`.
/// Returns (version tag, unpacked file names).
pub async fn stage_engine(kind: EngineKind, staging: &Path) -> Result<(String, Vec<String>), String> {
    let rel = engine_latest(kind).await?;
    let asset = engine_asset(kind, &rel).ok_or_else(|| format!("{}: no Windows x64 build in {}", kind.label(), rel.tag))?;
    let expected = expected_sha256(&rel, &asset).await;
    let data = download(&asset.url, MAX_DOWNLOAD).await?;
    match &expected {
        Some(want) => {
            let got = sha256_hex(&data);
            if &got != want {
                return Err(format!("{}: checksum mismatch, download rejected", asset.name));
            }
        }
        None => return Err(format!("{}: the release has no checksum — not installing an unverified file", asset.name)),
    }
    let staging = staging.to_path_buf();
    let tgz = asset.name.ends_with(".tar.gz");
    let files = tokio::task::spawn_blocking(move || if tgz { unpack_tgz(kind, &data, &staging) } else { unpack(kind, &data, &staging) })
        .await
        .map_err(|e| e.to_string())??;
    Ok((rel.tag, files))
}

fn unpack(kind: EngineKind, zip_bytes: &[u8], staging: &Path) -> Result<Vec<String>, String> {
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir_all(staging).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(Cursor::new(zip_bytes)).map_err(|e| format!("bad archive: {e}"))?;
    let mut got = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        if f.is_dir() {
            continue;
        }
        let base = f.name().rsplit('/').next().unwrap_or("").to_string();
        if wanted_files(kind).contains(&base.as_str()) && !got.contains(&base) {
            let mut out = std::fs::File::create(staging.join(&base)).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
            got.push(base);
        }
    }
    finish_unpack(kind, staging, got)
}

/// sing-box for Linux comes as .tar.gz.
fn unpack_tgz(kind: EngineKind, bytes: &[u8], staging: &Path) -> Result<Vec<String>, String> {
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir_all(staging).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)));
    let mut got = Vec::new();
    for entry in ar.entries().map_err(|e| format!("bad archive: {e}"))? {
        let mut e = entry.map_err(|e| format!("bad archive: {e}"))?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let path = e.path().map_err(|e| e.to_string())?.into_owned();
        let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if wanted_files(kind).contains(&base.as_str()) && !got.contains(&base) {
            let mut out = std::fs::File::create(staging.join(&base)).map_err(|e| e.to_string())?;
            std::io::copy(&mut e, &mut out).map_err(|e| e.to_string())?;
            got.push(base);
        }
    }
    finish_unpack(kind, staging, got)
}

fn finish_unpack(kind: EngineKind, staging: &Path, got: Vec<String>) -> Result<Vec<String>, String> {
    let exe = kind.exe_name();
    if !got.iter().any(|g| g == exe) {
        return Err(format!("{exe} not found in the archive"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(staging.join(exe), std::fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    let _ = staging;
    Ok(got)
}

/// Move `files` from `from` into `to`, keeping the replaced ones in `backup`.
pub fn swap_in(files: &[String], from: &Path, to: &Path, backup: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(backup);
    std::fs::create_dir_all(backup).map_err(|e| e.to_string())?;
    for f in files {
        let dst = to.join(f);
        if dst.exists() {
            std::fs::rename(&dst, backup.join(f)).map_err(|e| format!("{f}: {e}"))?;
        }
        std::fs::rename(from.join(f), &dst).map_err(|e| format!("{f}: {e}"))?;
    }
    Ok(())
}

/// Undo `swap_in`: restore the backed-up files (or remove new ones that had no predecessor).
pub fn roll_back(files: &[String], to: &Path, backup: &Path) {
    for f in files {
        let b = backup.join(f);
        let dst = to.join(f);
        if b.exists() {
            let _ = std::fs::remove_file(&dst);
            let _ = std::fs::rename(&b, &dst);
        } else {
            let _ = std::fs::remove_file(&dst);
        }
    }
}

// ---------------------------------------------------------------- Mosaic itself

#[derive(Clone, Debug, Serialize)]
pub struct AppUpdate {
    pub version: String,
    pub page: String,
    pub notes: String,
    pub installer: Option<String>,
}

pub fn app_repo() -> String {
    std::env::var("MOSAIC_UPDATE_REPO").unwrap_or_else(|_| APP_REPO.to_string())
}

pub async fn check_app() -> Result<Option<AppUpdate>, String> {
    let rel = latest_release(&app_repo()).await?;
    if !is_newer(&rel.tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let installer = rel.assets.iter().find(|a| a.name.ends_with("-setup.exe")).map(|a| a.url.clone());
    Ok(Some(AppUpdate { version: rel.tag.trim_start_matches('v').to_string(), page: rel.html_url, notes: rel.notes, installer }))
}

/// Download the installer of the latest release (checksum verified) to `dir`.
pub async fn download_app_installer(dir: &Path) -> Result<PathBuf, String> {
    let rel = latest_release(&app_repo()).await?;
    let asset = rel.assets.iter().find(|a| a.name.ends_with("-setup.exe")).cloned().ok_or("the release has no installer")?;
    let want = expected_sha256(&rel, &asset).await.ok_or("the release has no checksum for the installer")?;
    let data = download(&asset.url, MAX_DOWNLOAD).await?;
    if sha256_hex(&data) != want {
        return Err("installer checksum mismatch".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(&asset.name);
    std::fs::write(&path, data).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("Xray 25.10.15 (Xray, Penetrates Everything.)"), Some(vec![25, 10, 15]));
        assert_eq!(parse_version("sing-box version 1.12.4"), Some(vec![1, 12, 4]));
        assert!(is_newer("v1.13.0", "sing-box version 1.12.9"));
        assert!(!is_newer("v1.12.9", "1.12.9"));
        assert!(is_newer("v0.4.0", "0.3.0"));
        assert!(!is_newer("v0.3", "0.3.0"));
    }

    #[test]
    fn sha() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn assets() {
        let v: Value = serde_json::from_str(
            r#"{"tag_name":"v1.12.4","html_url":"u","body":"","assets":[
                {"name":"sing-box-1.12.4-windows-amd64-legacy-windows-7.zip","browser_download_url":"a"},
                {"name":"sing-box-1.12.4-windows-amd64.zip","browser_download_url":"b","digest":"sha256:ABC"},
                {"name":"sing-box-windows-amd64.zip","browser_download_url":"x"},
                {"name":"Xray-windows-64.zip","browser_download_url":"c"}]}"#,
        )
        .unwrap();
        let rel = parse_release(&v).unwrap();
        assert_eq!(engine_asset(EngineKind::Singbox, &rel).unwrap().url, "b");
        assert_eq!(engine_asset(EngineKind::Xray, &rel).unwrap().url, "c");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn assets_linux() {
        let v: Value = serde_json::from_str(
            r#"{"tag_name":"v1.12.4","html_url":"u","body":"","assets":[
                {"name":"sing-box-1.12.4-linux-amd64-glibc.tar.gz","browser_download_url":"a"},
                {"name":"sing-box-1.12.4-linux-amd64.tar.gz","browser_download_url":"b"},
                {"name":"Xray-linux-64.zip","browser_download_url":"c"}]}"#,
        )
        .unwrap();
        let rel = parse_release(&v).unwrap();
        assert_eq!(engine_asset(EngineKind::Singbox, &rel).unwrap().url, "b");
        assert_eq!(engine_asset(EngineKind::Xray, &rel).unwrap().url, "c");
    }
}
