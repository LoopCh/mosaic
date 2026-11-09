# Mosaic 0.2

A multi-engine proxy/VPN client. Routing, DNS, statistics and node selection
live in a small Rust **hub**; Xray and sing-box are interchangeable **engines**
that only carry traffic to servers. Rules can send different traffic to nodes on
different engines at the same time.

Interface languages: English, Русский (automatic, switchable in Settings).

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
