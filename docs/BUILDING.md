# Building Mosaic from source

Most people do not need this: ready builds are on the [Releases](https://github.com/LoopCh/mosaic/releases) page,
and every push builds all platforms in GitHub Actions (Actions → a run → Artifacts).

## Windows

Requirements: Windows 10/11 x64, [Rust](https://rustup.rs) with the MSVC toolchain
(Visual Studio Build Tools, “Desktop development with C++”). WebView2 ships with Windows.

```powershell
# 1. Engines (Xray + sing-box), geoip.dat / geosite.dat and wintun.dll → .\engines
powershell -ExecutionPolicy Bypass -File scripts\get-engines.ps1

# 2. Core tests
cargo test -p mosaic-core

# 3. Run the app (stable build)
cargo run --release -p mosaic

```

TUN mode needs administrator rights: run `target\release\mosaic.exe` as administrator.

**Without installing Rust:** fork the repository; every push builds the installer and a portable zip
(Actions → a run → Artifacts).

### CLI

```powershell
mosaic-cli import "vless://…"      mosaic-cli sub https://example.com/sub
mosaic-cli list                     mosaic-cli test
mosaic-cli run                      # system proxy, Ctrl+C to quit
mosaic-cli run --proxy-only         # local proxy only
mosaic-cli run --tun                # TUN via the sing-box adapter (admin console)
mosaic-cli run --tun --native       # native TUN (native-tun build, admin console)
```


## Linux

Requirements: Rust and (Debian/Ubuntu names) `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf libssl-dev`.

```sh
bash ./scripts/get-engines-linux.sh        # Xray, sing-box, geo files → ./engines
cargo test -p mosaic-core --features native-tun
cargo run --release -p mosaic
```

Packages (`.deb`, `.rpm`, AppImage): `cd app/src-tauri && npx @tauri-apps/cli@2 build`.
TUN asks for the password once to give the TUN helper `cap_net_admin`; Mosaic itself never runs as root.

## Android

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


## Releasing

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


## Repository layout

```
crates/mosaic-core   hub: IR, parsers, config compilers, data plane (TCP+UDP), geo, DNS, native TUN
crates/mosaic-cli    command-line client
app/src-tauri        Tauri 2 shell
app/ui               UI (HTML/CSS/JS, no bundler) + i18n.js
scripts              engine download
```


## Where things are on disk

| Path | What |
|---|---|
| `%APPDATA%\Mosaic\state.json` | nodes, subscriptions, rules, settings |
| `%APPDATA%\Mosaic\run\*.json` | generated engine configs |
| `engines\` | xray.exe, sing-box.exe, geoip.dat, geosite.dat, wintun.dll |

On Linux the data folder is `~/.config/mosaic`, on Android the app's private storage. Logs: `<data>/logs/mosaic.log`.
