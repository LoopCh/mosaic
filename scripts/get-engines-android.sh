#!/usr/bin/env bash
# Download the Android (arm64) builds of Xray and sing-box plus geo files into
# the Android project:
#   android/app/src/main/jniLibs/arm64-v8a/libxray.so, libsingbox.so
#   android/app/src/main/assets/geo/geoip.dat, geosite.dat
# Engines are executables; Android only allows running them from the app's
# native library folder, hence the lib*.so names.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
source "$root/scripts/lib-fetch.sh"
libs="$root/android/app/src/main/jniLibs/arm64-v8a"
geo="$root/android/app/src/main/assets/geo"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$libs" "$geo"

fetch_asset XTLS/Xray-core '^Xray-android-arm64-v8a\.zip$' "$tmp/xray.zip"
unzip -q -o "$tmp/xray.zip" -d "$tmp/xray"
install -m 755 "$tmp/xray/xray" "$libs/libxray.so"
cp "$tmp/xray/geoip.dat" "$tmp/xray/geosite.dat" "$geo/"

fetch_asset SagerNet/sing-box '^sing-box-[0-9.]+-android-arm64\.tar\.gz$' "$tmp/sb.tar.gz"
mkdir -p "$tmp/sb" && tar -xzf "$tmp/sb.tar.gz" -C "$tmp/sb"
install -m 755 "$(find "$tmp/sb" -type f -name sing-box | head -1)" "$libs/libsingbox.so"

ls -la "$libs" "$geo"
