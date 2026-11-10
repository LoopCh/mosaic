# Mosaic 0.9

A multi-engine proxy/VPN client. Routing, DNS, statistics and node selection
live in a small Rust **hub**; Xray and sing-box are interchangeable **engines**
that only carry traffic to servers. Rules can send different traffic to nodes on
different engines at the same time.

Interface languages: English, Русский (automatic, switchable in Settings).

Docs: [architecture and tools](docs/ARCHITECTURE.md) · [project overview, history, plans](docs/PROJECT.md) (Russian).

## Arch Linux (AUR)

```sh
paru -S mosaic-vpn-bin   # or yay -S mosaic-vpn-bin
```

Updates come with `paru -Syu`. See [packaging/aur](packaging/aur/README.md).

## 0.9.2

- Native TUN: some HTTPS sites dropped the connection right after the TLS handshake started
  (`NS_ERROR_NET_RESET`, “unexpected eof”). The stack announced no MSS, so apps sent the TLS ClientHello in
  536-byte pieces and Mosaic forwarded them one by one; some servers and SNI routers reject that. Now the
  stack announces MSS 1440 and a 64 KiB window, and the app's first TLS message is forwarded in one piece,
  exactly as with a direct connection. Also helps SNI detection when the browser bypasses Mosaic's DNS.
- The TUN journal no longer reports multicast chatter (mDNS, LLMNR, SSDP) and idle UDP flows as errors.

## 0.9.1

- Fixed “Too many open files” on Linux after some time in TUN mode: Mosaic raises its open-files limit at
  start (like Xray and sing-box do), and TCP connections are closed after 5 minutes without data
  (30 s once one side has finished), so sockets no longer pile up.

## New in 0.9 — subscriptions, more formats, connections → rules

- **Subscription info.** Nodes are grouped by subscription. Each group header shows the title the provider
  sends (`profile-title` / `content-disposition`), traffic used and left with a bar
  (`subscription-userinfo`), the expiry date with days left (highlighted when ≤ 3 days or expired), the last
  update and its error. Every subscription can be updated (⟳) or deleted on its own; groups collapse.
- **Import Clash / mihomo YAML and sing-box JSON.** Paste a config or add a subscription that returns one:
  `proxies:` (ss, vmess, vless incl. REALITY, trojan, hysteria2, tuic, wireguard; ws/grpc/httpupgrade/xhttp)
  and sing-box `outbounds`/`endpoints` are converted to nodes. Groups, direct/reject entries are skipped;
  unsupported types (ssr, plugins) are listed as errors.
- **Nodes:** sort as added / by latency / by name (remembered), “Test all”, search keeps working across groups.
- **Connections → rules.** Select one or many connections (checkboxes, select all, or tap a card on phones),
  or press “+” on a row, and choose what the rule matches: domain with subdomains (registrable domain,
  `bbc.co.uk` not `co.uk`), exact domain, application, IP or port. Values are deduplicated, ones already in
  rules are skipped, the list is editable; choose where to send it and whether to put it first. The rule
  applies instantly.
- **Connections table:** sort by any column (click the header; a sort menu on phones), destination shown as
  `host:port`, host only, or domain only.

## New in 0.8 — native TUN on Linux

- **Mosaic's own TUN stack on Linux** (Settings → TUN → “Mosaic TUN”), the same stack as on Windows and
  Android: no extra sing-box process, every rule (rules by application too) applies instantly, and
  firewalls (ufw, firewalld) do not interfere.
- How it works without root: a tiny helper, `mosaic-tun-helper` (shipped next to Mosaic), runs from a copy
  with `cap_net_admin` (granted with the usual one-time password prompt). It creates a TUN interface owned
  by you, sets the routes (0/1 + 128/1 into the tunnel, node servers around it, the system's DNS servers
  into it) and the interface DNS in systemd-resolved (`~.`), and removes everything when Mosaic disconnects,
  quits or crashes. Mosaic attaches to the interface without any privileges.
- Mosaic's own direct connections are bound to the physical interface (`SO_BINDTODEVICE`, Linux 5.7+), so
  they never loop into the tunnel; engines reach their servers by bypass routes.
- The sing-box adapter stays available as the alternative stack.
- Building locally: `cargo build --release -p mosaic-tun-helper`, then copy it to
  `app/src-tauri/binaries/mosaic-tun-helper-x86_64-unknown-linux-gnu` before `tauri build` (CI does this).

## New in 0.7.6

- Applying rules in TUN mode (sing-box adapter: Linux, and Windows with that stack) no longer restarts the
  tunnel: rules are swapped in the hub without dropping connections; the adapter is rebuilt only when the
  rules *by application* change.

## New in 0.7.5

- Windows DNS leak protection is **fail-closed**: if the filters cannot be installed, or a live check
  shows that the system DNS does not go through the tunnel (a never-seen name must resolve to a fake IP
  198.18.x.x), Mosaic does not connect and says why — instead of running with a DNS leak or no DNS.
  DNS to the TUN's own DNS address (172.19.0.2) is always allowed in addition to the TUN adapter.
- Windows TUN adds a default route on the tunnel and flushes the DNS cache, so the system DNS uses the
  tunnel's DNS server. If a name resolution policy (NRPT) sends all DNS elsewhere — Tailscale's
  “Use Tailscale DNS settings”, corporate VPNs — Mosaic names the cause instead of connecting with a leak.
- sing-box TUN adapter: if IPv6 is turned off in the system ("set ipv6 address: Element not found"),
  it is restarted with IPv4 only instead of failing.
- Native TUN (Windows, Android) logs what it sees every 5 s while traffic changes: packets in/out,
  TCP/UDP flows, DNS queries and the first errors of failed flows — to find where traffic stops.

## New in 0.7.4

- Linux TUN uses sing-box's gVisor stack: with the system/mixed stack, packets went through the INPUT
  firewall chain, and ufw/firewalld (enabled by default on CachyOS, Fedora and others) dropped them —
  the interface was up but nothing passed.
- Linux: a warning in Logs when Mosaic runs as root (the system proxy then applies to root, not to the
  desktop user). Run Mosaic as a normal user; TUN asks for the password itself.
- The “Install” update button works again (it failed after the confirmation dialog).
- Linux with systemd-resolved: the one-time password prompt also installs a polkit rule
  (`/etc/polkit-1/rules.d/49-mosaic.rules`, only for your user) so that connecting and switching
  nodes no longer asks “reset name resolution settings” / “flush DNS caches”. Delete the file to undo.

## New in 0.7.3

- Fix: 0.7.2 put an unknown `disable_color` key into sing-box configs, so sing-box (engine and TUN
  adapter) refused to start on every platform. Colours are now turned off with the `--disable-color` flag.

## New in 0.7.2

- Linux/Android: engines and the TUN adapter are stopped with SIGTERM first (SIGKILL after 3 s), so
  sing-box removes its routes and `ip rule` entries (table 2022, priorities 9000–9010) on disconnect.
- Note: DPI-bypass tools that rewrite outgoing packets (zapret, GoodbyeDPI…) can break connections to
  proxy servers; exclude the node servers from them or turn them off while Mosaic is connected.

## New in 0.7.1

- Linux TUN: node servers are now routed around the TUN (`route_exclude_address`) and engines get
  their servers as IP addresses — engine traffic could loop back into the tunnel (seen as
  `172.19.0.1 → server: connection reset` and zero download).
- Linux TUN: the capability is set on a separate copy (`~/.config/mosaic/bin/sing-box-tun`), not on the
  engine itself; a copy left with the capability by 0.7.0 is replaced automatically.
- Cleaner logs: no Xray access log, no colour codes from sing-box.

## New in 0.7 — Linux

- **Packages:** `.deb` (Debian, Ubuntu, Mint…), `.rpm` (Fedora, openSUSE…) and a portable `.AppImage`,
  x86_64, built by `.github/workflows/linux.yml`; engines bundled.
- **System proxy** on GNOME-based desktops (gsettings) and KDE Plasma (kioslaverc). Elsewhere use
  the Proxy mode and set 127.0.0.1:7890 in apps.
- CI and releases build only the app; the console client (`crates/mosaic-cli`) stays for development
  (`cargo run -p mosaic-cli`).
- **TUN** through the sing-box adapter, without running Mosaic as root: the first time, Mosaic asks
  for your password once (polkit) and gives its own copy of sing-box the `cap_net_admin` capability.
  Needs `setcap` (libcap2-bin / libcap) and `pkexec` (polkit) — present on regular desktops.
- Engines are copied from the package to `~/.config/mosaic/bin` (executable, can carry the capability);
  engine updates work on Linux too (tar.gz).
- **Rules by application** in system-proxy and proxy modes (owner from /proc), by program name
  (`telegram-desktop`, `firefox`); in TUN the rules are passed to sing-box as on Windows.
- Autostart (`~/.config/autostart/mosaic.desktop`), tray, single instance (a socket in `$XDG_RUNTIME_DIR`).
- The update banner opens the release page (no installer to run on Linux).

**Testing on a Windows PC:** Windows 11 runs Linux GUI apps in WSL2 (WSLg): `wsl --install -d Ubuntu`, then in
Ubuntu `sudo apt install ./Mosaic_0.7.0_amd64.deb` (copy the file from `/mnt/c/Users/<you>/Downloads`) and
run `mosaic`. System proxy has no effect there (no GNOME session) — use Proxy mode or TUN (TUN in WSL changes
only the WSL network). A VirtualBox VM with Ubuntu desktop gives the full picture.

## New in 0.6.1

- Android: the app picker opens at once and loads icons lazily (served by the app, not passed through
  the bridge) — it froze on phones with many apps.
- Android: **rules by app** work in VPN mode — rule type “App (package)”, e.g. `org.telegram.messenger → node X`.
  The owner of each connection comes from Android (`getConnectionOwnerUid`, Android 10+); the Connections
  tab shows the package name.
- Android: in-app update check and banner removed (new versions come as an APK).

## New in 0.6

- **Android — which apps use the VPN** (Settings → Apps): all apps, all except the chosen ones, or only
  the chosen ones; app picker with icons and search.
- **Android — Quick Settings tile:** connect/disconnect from the notification shade.
- **Connect automatically** on Android: when the phone starts and when Mosaic is opened. Android's own
  “Always-on VPN” (system VPN settings) is supported: the system starts Mosaic itself.
- **Import from the clipboard** on Android and **QR code scanning** (Nodes → Import).
- **Two latency numbers:** round trip through the node (comparable with “ping” in speed tests) and, smaller,
  the time a new connection takes (handshakes with the node included). Smart Connect still compares new-connection times.

## New in 0.5.1

- Deleting a subscription works again: confirmations are now Mosaic's own dialogs (the system
  `confirm()` is silently refused by Android's WebView and is unreliable in embedded web views).
- Phones: nodes, rules and connections are laid out as cards so every property is visible.
- Android wording (no mentions of Windows or administrator rights).

## New in 0.5 — Android (first version)

- **Android app** (arm64, Android 8+): the same interface as on the desktop, adapted to phones
  (bottom navigation), and the same Rust core (`crates/mosaic-android`, a JNI library).
- **VPN mode:** Android's VpnService creates the interface, Mosaic's own TCP/IP stack (the native TUN
  from Windows) runs on it with fake-IP DNS, sniffing and all rules. Mosaic excludes itself from the
  VPN, so engines and direct connections never loop back into the tunnel.
- **Proxy mode:** only the local SOCKS5/HTTP proxy (127.0.0.1:7890, or the LAN with “allow devices”).
- Xray and sing-box run as on the desktop (Android builds, packaged as `libxray.so` / `libsingbox.so`).
  Node servers are resolved by Mosaic and passed to the engines as IPs (Go cannot use Android's DNS).
- A notification with “Disconnect” keeps the connection alive when the app is closed.
- Engines are updated together with the app on Android (the system does not allow running downloaded files).
- Desktop: “Restore bundled versions” is shown only when updated engines are installed.

Not in this version yet: x86_64/armv7 builds, in-app APK updates
(the banner opens the release page).

### Android build

CI (`.github/workflows/android.yml`) builds `Mosaic_<version>_android-arm64.apk` on every push (artifact)
and attaches it to releases. Locally: Rust + `cargo install cargo-ndk`, Android SDK/NDK, then

```
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs build --release -p mosaic-android
bash ./scripts/get-engines-android.sh
cd android && gradle assembleRelease
```

**Signing.** Without secrets the APK is signed with a throwaway key, so each build must be installed
over a clean uninstall. For updates that install over each other, create a key once and add four repo
secrets (`ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`):

```
keytool -genkeypair -v -keystore mosaic.jks -alias mosaic -keyalg RSA -keysize 4096 -validity 10000
base64 -w0 mosaic.jks   # → ANDROID_KEYSTORE_B64
```

Keep `mosaic.jks` safe: an app signed with a lost key can never be updated.

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
git tag v0.8.0
git push origin v0.8.0
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
