#!/usr/bin/env bash
# Download the Android (arm64) builds of Xray and sing-box plus geo files into
# the Android project:
#   android/app/src/main/jniLibs/arm64-v8a/libxray.so, libsingbox.so
#   android/app/src/main/assets/geo/geoip.dat, geosite.dat
# Engines are executables; Android only allows running them from the app's
# native library folder, hence the lib*.so names.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
libs="$root/android/app/src/main/jniLibs/arm64-v8a"
geo="$root/android/app/src/main/assets/geo"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$libs" "$geo"

auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $GITHUB_TOKEN"); fi
latest() { curl -fsSL "${auth[@]}" "https://api.github.com/repos/$1/releases/latest" | jq -r .tag_name; }

xver="$(latest XTLS/Xray-core)"
echo "Xray $xver"
curl -fsSL -o "$tmp/xray.zip" "https://github.com/XTLS/Xray-core/releases/download/$xver/Xray-android-arm64-v8a.zip"
unzip -q -o "$tmp/xray.zip" -d "$tmp/xray"
install -m 755 "$tmp/xray/xray" "$libs/libxray.so"
cp "$tmp/xray/geoip.dat" "$tmp/xray/geosite.dat" "$geo/"

sver="$(latest SagerNet/sing-box)"
sv="${sver#v}"
echo "sing-box $sver"
curl -fsSL -o "$tmp/sb.tar.gz" "https://github.com/SagerNet/sing-box/releases/download/$sver/sing-box-$sv-android-arm64.tar.gz"
tar -xzf "$tmp/sb.tar.gz" -C "$tmp"
install -m 755 "$tmp/sing-box-$sv-android-arm64/sing-box" "$libs/libsingbox.so"

ls -la "$libs" "$geo"
