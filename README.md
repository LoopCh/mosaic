# Mosaic 0.4

A multi-engine proxy/VPN client. Routing, DNS, statistics and node selection
live in a small Rust **hub**; Xray and sing-box are interchangeable **engines**
that only carry traffic to servers. Rules can send different traffic to nodes on
different engines at the same time.

Interface languages: English, Русский (automatic, switchable in Settings).

## New in 0.4

- **Installer (Windows):** `Mosaic_<version>_x64-setup.exe` (NSIS, per-user install, no admin needed to
  install, English/Russian). Engines are bundled. A portable zip is published next to it.
- **Engine updates** (Engines → “Check for updates”): downloads the latest Xray / sing-box release from
  GitHub, **verifies its SHA-256**, unpacks it into a staging folder, runs `version`, then swaps it in and
  reconnects. If anything fails, the previous engine is restored automatically. Updated engines live in
  `%APPDATA%\Mosaic\engines` and take priority over the bundled ones; “Restore bundled versions” removes them.
- **Mosaic updates:** checked once a day (Settings → General, can be turned off). A banner offers
  “Install”: the installer is downloaded, its checksum verified, and it runs after Mosaic closes.
  Nothing is installed without your click.
- Native TUN is part of the regular build now.

### Releasing

```
git tag v0.4.0
git push origin v0.4.0
```

The `release` workflow builds the installer, the portable zip and `SHA256SUMS`, and publishes a GitHub
release. The in-app updater looks at the latest (non-pre-)release of `LoopCh/mosaic` and refuses files
without a checksum. Versions to bump together: `Cargo.toml` of mosaic-core, mosaic-cli, the app and
`tauri.conf.json`.

Build the installer locally: `scripts/get-engines.ps1`, then in `app/src-tauri`
`npx @tauri-apps/cli@2 build`.

## What's new in 0.2

- **UDP through the hub** (SOCKS5 UDP ASSOCIATE): UDP follows the same rules as TCP.
  In TUN mode, switching nodes is now instant and no longer restarts the TUN adapter.
- **GeoIP / Geosite rules** using the standard `geoip.dat` / `geosite.dat`
  (read by a built-in parser, only the codes your rules use are loaded).
- **Country-neutral presets:** “My country direct” for any country (its domain zone
  + GeoIP), “Block ads” (`geosite:category-ads-all`).
- **IP rules for domains:** optional “resolve if no domain rule matched” strategy.
- **Native TUN stack (experimental, Windows):** Mosaic’s own TUN — wintun + userspace
  TCP/IP (ipstack), built-in fake-IP DNS, TLS SNI / HTTP Host sniffing, bypass routes
  for node servers. Shipped as a separate build so it can never break the stable one.
- English/Russian UI, English core messages.
- **DNS leak protection (0.2.1, Windows):** in TUN mode Mosaic adds Windows Filtering Platform
  rules that block DNS (port 53) everywhere except the TUN interface; Mosaic and its engines are
  exempt. The WFP session is dynamic, so Windows removes the rules automatically even if Mosaic
  crashes. Toggle: Settings → “DNS leak protection”. Status is shown on the Home screen.

## New in 0.3

- **Smart Connect** (Nodes → switch, or tray → Nodes → Auto): keeps the fastest working node
  selected. Checks the active node every 20 s and all nodes every 3 min; switches instantly when the
  active node fails or another is clearly faster (>30 % + 50 ms). Choosing a node by hand turns it off.
- **Per-application rules:** rule type “Application (.exe)”, e.g. `qbittorrent.exe → Direct`,
  `telegram.exe → node X`. Works in system-proxy, proxy and native-TUN modes (owner of the socket
  via the Windows IP helper tables). With the sing-box TUN adapter the rules are passed into the
  adapter (targets Direct/Block/node; “Selected node” is handed to the hub).
- **Connections show the application** that opened each connection.
- **Scheduled subscription refresh:** every 6/12/24 h (Settings → Advanced), also checked at start.
- **Single instance:** launching Mosaic again just opens the running window.

## New in 0.2.3

- **Three modes:** System proxy · Proxy (local proxy only, Windows settings untouched) · TUN.
- **Allow devices in the local network:** phones/TVs can use this PC as a SOCKS5/HTTP proxy
  (the Home screen shows the address). No password — trusted networks only.
- **Mode on start:** last used / system proxy / proxy / TUN (TUN without admin rights falls back
  to the system proxy).
- **Settings reorganised** into General · Local proxy · TUN mode · DNS · Advanced, each option
  with a plain-language description.

## New in 0.2.2

- **DNS through the tunnel:** in native TUN, MX/TXT/SRV/PTR… queries are forwarded to the remote DNS
  (default 1.1.1.1, Settings → Remote DNS) through the active node; AAAA/HTTPS/SVCB stay empty so
  apps keep using fake-IP routing. The sing-box adapter uses the same remote DNS.
- **GeoIP resolution choice:** Settings → “Where to resolve names for GeoIP / IP rules”:
  through the VPN (private, default) or locally (more accurate GeoIP).
- **Tray:** open, connect/disconnect, switch node, quit. Double-click opens the window; closing the
  window keeps Mosaic in the tray (Settings → Application).
- **Autostart** (minimized to the tray) and **auto-connect**. Turn autostart on while Mosaic runs as
  administrator: it then registers an elevated logon task, so TUN works right after logon.
  Otherwise it uses the per-user Run key (system-proxy mode only).

## Quick start (Windows)

Requirements: Windows 10/11 x64, [Rust](https://rustup.rs) with the MSVC toolchain
(Visual Studio Build Tools, “Desktop development with C++”). WebView2 ships with Windows.

```powershell
# 1. Engines (Xray + sing-box), geoip.dat / geosite.dat and wintun.dll → .\engines
powershell -ExecutionPolicy Bypass -File scripts\get-engines.ps1

# 2. Core tests
cargo test -p mosaic-core

# 3. Run the app (stable build)
cargo run --release -p mosaic

# 3b. Or the build with the experimental native TUN
cargo run --release -p mosaic --features mosaic-core/native-tun
```

TUN mode needs administrator rights: run `target\release\mosaic.exe` as administrator.

**Without installing Rust:** push the repository to GitHub. The workflow builds two
portable archives (Actions → Artifacts): `Mosaic-windows-x64` (stable) and
`Mosaic-windows-x64-native-tun` (experimental).

### CLI

```powershell
mosaic-cli import "vless://…"      mosaic-cli sub https://example.com/sub
mosaic-cli list                     mosaic-cli test
mosaic-cli run                      # system proxy, Ctrl+C to quit
mosaic-cli run --proxy-only         # local proxy only
mosaic-cli run --tun                # TUN via the sing-box adapter (admin console)
mosaic-cli run --tun --native       # native TUN (native-tun build, admin console)
```

## Test checklist for 0.2

1. **UDP in TUN (sing-box adapter).** Settings → TUN implementation: sing-box. Connect in TUN.
   Open a site that uses UDP (e.g. a WebRTC/STUN test or an online game); *Connections*
   shows `UDP` rows with rule, node and engine.
2. **Instant switch in TUN.** Change the node while connected: no reconnect, the TUN stays up,
   new TCP and UDP flows go through the new node.
3. **GeoIP / Geosite.** Rules → “+ My country direct” → pick your country → Apply. A site in
   your country’s domain zone shows the `domain_suffix` rule; with Settings → “Resolve if no
   domain rule matched”, a site hosted in your country but under `.com` shows the `geoip` rule.
   “+ Block ads” → `doubleclick.net` is blocked.
4. **Native TUN (native-tun build).** Settings → TUN implementation: native → Save → connect in TUN
   as administrator. Check `curl ifconfig.me` in a console, browsing, a UDP app, and that
   closing the app restores normal internet.
5. **Language.** Settings → Interface language.
6. **DNS leaks.** In TUN mode run the extended test on dnsleaktest.com: only resolvers on the VPN
   side should appear, none from your ISP. Home shows “DNS protected”.

If something fails: send the *Logs* tab and `%APPDATA%\Mosaic\run\*.json`.

## Known limitations

- Native TUN: Windows only; IPv4 DNS answers only (IPv6 traffic is captured but apps get IPv4).
- DNS leak protection also blocks DNS to local resolvers (127.0.0.1:53); turn it off if you run one.
- Geosite `regexp:` entries are skipped (no regex engine in the core yet).
- System-proxy mode carries UDP only for apps that speak SOCKS5 UDP.
- No tray/autostart yet; Linux/macOS/mobile are later stages.

## Layout

```
crates/mosaic-core   hub: IR, parsers, config compilers, data plane (TCP+UDP), geo, DNS, native TUN
crates/mosaic-cli    command-line client
app/src-tauri        Tauri 2 shell
app/ui               UI (HTML/CSS/JS, no bundler) + i18n.js
scripts              engine download
```

## Где что лежит / Where things are

| Path | What |
|---|---|
| `%APPDATA%\Mosaic\state.json` | nodes, subscriptions, rules, settings |
| `%APPDATA%\Mosaic\run\*.json` | generated engine configs |
| `engines\` | xray.exe, sing-box.exe, geoip.dat, geosite.dat, wintun.dll |

License: GPL-3.0-or-later.
