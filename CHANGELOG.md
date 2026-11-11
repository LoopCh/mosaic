# Changelog

Newest first. Versions before 0.5 cover Windows only.

## 0.11.1 — detailed logs, a kill switch that never pretends

- **Verbose log** (Logs → “every connection”, off by default): every connection is written — app, destination,
  rule, route (direct / node / engine), how long it lasted, traffic, how it ended. Without it, failures are still
  logged: a connection that got nothing back leaves one warning per host and error a minute.
- **WebRTC in the log**: what the protection does with each STUN server (sent through the node, blocked because
  no node is active, or — with the protection off — sent direct), once a minute per server. At connect the log
  says whether the protection applies at all: it works only in TUN mode; with a proxy, browsers send WebRTC
  past Mosaic.
- **Session summary** at every connect: mode, TUN stack, node, kill switch, WebRTC and DNS protection, QUIC,
  LAN, final route, DNS servers, rules.
- **Kill switch, fail-closed on Linux**: with the sing-box adapter it cannot block anything, so Mosaic no longer
  connects with it on (before, it silently did nothing); use the Mosaic TUN implementation, or turn it off.
  In proxy modes the log says it is not active.
- **Kill switch, checked**: after the native TUN is up the routing table itself is checked (IPv4 into the tunnel,
  IPv6 when the system has IPv6 internet) — if traffic would go around the tunnel, there is no connection, and
  the routing table goes to the log. At start Mosaic says whether a block from the last session is really in
  force, and warns if it is gone (after a reboot) or incomplete.
- **TUN helper**: SIGTERM / SIGINT / SIGHUP (`pkill`, a closed terminal, logging out) are handled like Mosaic
  dying — the kill switch holds, otherwise everything is cleaned up. What it did at the end is written to
  `logs/mosaic.log` even when Mosaic is already gone.
- **Leftovers**: a blocking interface from an earlier session is removed when the new connection does not use
  Mosaic's TUN (system proxy, sing-box adapter) — before, direct traffic and WebRTC calls stayed broken in
  those modes. A leftover interface without the kill switch is removed at start.
- **Diagnostic report**: kill switch state and the routing table (`ip route`, `ip -6 route`, `ip rule`, links) on
  Linux, the recent failed connections, and the log with connection lines capped so they do not push out the
  rest. Public IPv6 addresses are now hidden as well.

## 0.11 — kill switch, grouped connections, diagnostics

- **Kill switch** (Settings → TUN, off by default). If the tunnel goes down without you disconnecting —
  Mosaic crashed or was killed, a reconnect after a change failed — traffic is blocked instead of leaking;
  the local network keeps working. Home shows “Internet blocked” with “Connect” and “Unblock”; the tray icon
  turns amber. Disconnecting or quitting lifts the block.
  * Windows: persistent firewall (WFP) filters — only Mosaic, its engines, the tunnel, loopback, LAN and DHCP
    may reach the network; they survive a crash. The uninstaller lifts them (`mosaic.exe --unblock`).
  * Linux (native TUN stack): the TUN interface and its routes stay when Mosaic dies, so all traffic goes
    into a tunnel nobody reads; reconnects take the interface over without a gap.
  * Android: the system's own “Always-on VPN” + “Block connections without VPN”; Settings shows whether it is
    on and opens the VPN settings.
- **Grouped connections**: by app, domain, rule or outbound. Groups show the count, active and failed
  connections, traffic and their top domains/apps; unfold for details, select a whole group, or add it to
  the rules in one click (by app for app groups, by domain for domain groups). Phones get stacked cards.
- **Logs for diagnostics**: levels and sources (hub, tun, Xray, sing-box, check, killswitch…), search, filters
  “warnings” / “errors” with counters, local time. The log is also written to `logs/mosaic.log` in the data
  folder (rotated, 3 × 2 MiB), panics included. **Diagnostic report**: system, status, engines, settings,
  nodes (anonymised), rule summary and the latest 1500 log lines — node names, servers, keys, links,
  subscription URLs and public IP addresses are replaced, so it can be shared. Saved to Downloads and copied
  (desktop) or sent through the share menu (Android). Settings changes and connect results are logged.
- Home shows exact byte rates (no long fractions); the Smart Connect card on Nodes is folded away behind a
  toolbar button that also shows whether it is on.

## 0.10 — honest status, new Home

- **The node is checked before Mosaic connects.** Mosaic sends a test request through the selected node
  before it switches the system proxy or TUN on. If the node does not answer in 12 s, Mosaic does not connect
  (“The server does not answer”, with “Choose another node”, “Smart Connect”, “Try again”). With Smart
  Connect it takes the fastest node that answers instead. A dead node never leaves you without internet.
- **After connecting the node keeps being checked** (every 20 s). If it stops answering, Home, the tray icon
  and the sidebar turn amber: “No connection through the server” — instead of a green “connected” that lies.
- **New Home.** The status is the centre: a mosaic of 16 tiles that assemble step by step while connecting
  (“Starting engines → Checking the node → Turning on TUN → Checking DNS”), breathe when connected (brighter
  with more traffic), turn amber when the node fails and fall apart on errors. The node card opens a quick
  switcher (search, sorted by latency, “Test all”, Auto). Respects “reduce motion”.
- **Visible reconnects.** Changing the mode, a node's engine or a setting that needs a reconnect shows
  “Reconnecting…” with the steps, then “Mode “TUN”: applied · reconnected in 1.4 s”; changes that apply live
  say “applied without reconnecting”.
- **Tray and taskbar icon by state:** grey — off, amber — connecting or no answer, colour — connected.
- **WebRTC leak protection** (Settings → TUN, on by default): in TUN mode STUN requests always go through
  the node, even for sites with a “Direct” rule, so pages cannot learn your real IP.

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

## 0.9 — subscriptions, more formats, connections → rules

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

## 0.8 — native TUN on Linux

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

## 0.7.6

- Applying rules in TUN mode (sing-box adapter: Linux, and Windows with that stack) no longer restarts the
  tunnel: rules are swapped in the hub without dropping connections; the adapter is rebuilt only when the
  rules *by application* change.

## 0.7.5

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

## 0.7.4

- Linux TUN uses sing-box's gVisor stack: with the system/mixed stack, packets went through the INPUT
  firewall chain, and ufw/firewalld (enabled by default on CachyOS, Fedora and others) dropped them —
  the interface was up but nothing passed.
- Linux: a warning in Logs when Mosaic runs as root (the system proxy then applies to root, not to the
  desktop user). Run Mosaic as a normal user; TUN asks for the password itself.
- The “Install” update button works again (it failed after the confirmation dialog).
- Linux with systemd-resolved: the one-time password prompt also installs a polkit rule
  (`/etc/polkit-1/rules.d/49-mosaic.rules`, only for your user) so that connecting and switching
  nodes no longer asks “reset name resolution settings” / “flush DNS caches”. Delete the file to undo.

## 0.7.3

- Fix: 0.7.2 put an unknown `disable_color` key into sing-box configs, so sing-box (engine and TUN
  adapter) refused to start on every platform. Colours are now turned off with the `--disable-color` flag.

## 0.7.2

- Linux/Android: engines and the TUN adapter are stopped with SIGTERM first (SIGKILL after 3 s), so
  sing-box removes its routes and `ip rule` entries (table 2022, priorities 9000–9010) on disconnect.
- Note: DPI-bypass tools that rewrite outgoing packets (zapret, GoodbyeDPI…) can break connections to
  proxy servers; exclude the node servers from them or turn them off while Mosaic is connected.

## 0.7.1

- Linux TUN: node servers are now routed around the TUN (`route_exclude_address`) and engines get
  their servers as IP addresses — engine traffic could loop back into the tunnel (seen as
  `172.19.0.1 → server: connection reset` and zero download).
- Linux TUN: the capability is set on a separate copy (`~/.config/mosaic/bin/sing-box-tun`), not on the
  engine itself; a copy left with the capability by 0.7.0 is replaced automatically.
- Cleaner logs: no Xray access log, no colour codes from sing-box.

## 0.7 — Linux

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

## 0.6.1

- Android: the app picker opens at once and loads icons lazily (served by the app, not passed through
  the bridge) — it froze on phones with many apps.
- Android: **rules by app** work in VPN mode — rule type “App (package)”, e.g. `org.telegram.messenger → node X`.
  The owner of each connection comes from Android (`getConnectionOwnerUid`, Android 10+); the Connections
  tab shows the package name.
- Android: in-app update check and banner removed (new versions come as an APK).

## 0.6

- **Android — which apps use the VPN** (Settings → Apps): all apps, all except the chosen ones, or only
  the chosen ones; app picker with icons and search.
- **Android — Quick Settings tile:** connect/disconnect from the notification shade.
- **Connect automatically** on Android: when the phone starts and when Mosaic is opened. Android's own
  “Always-on VPN” (system VPN settings) is supported: the system starts Mosaic itself.
- **Import from the clipboard** on Android and **QR code scanning** (Nodes → Import).
- **Two latency numbers:** round trip through the node (comparable with “ping” in speed tests) and, smaller,
  the time a new connection takes (handshakes with the node included). Smart Connect still compares new-connection times.

## 0.5.1

- Deleting a subscription works again: confirmations are now Mosaic's own dialogs (the system
  `confirm()` is silently refused by Android's WebView and is unreliable in embedded web views).
- Phones: nodes, rules and connections are laid out as cards so every property is visible.
- Android wording (no mentions of Windows or administrator rights).

## 0.5 — Android (first version)

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

## 0.4

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

## 0.2

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

## 0.3

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

## 0.2.3

- **Three modes:** System proxy · Proxy (local proxy only, Windows settings untouched) · TUN.
- **Allow devices in the local network:** phones/TVs can use this PC as a SOCKS5/HTTP proxy
  (the Home screen shows the address). No password — trusted networks only.
- **Mode on start:** last used / system proxy / proxy / TUN (TUN without admin rights falls back
  to the system proxy).
- **Settings reorganised** into General · Local proxy · TUN mode · DNS · Advanced, each option
  with a plain-language description.

## 0.2.2

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
