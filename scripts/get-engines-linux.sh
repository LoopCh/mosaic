#!/usr/bin/env bash
# Download the Linux (x86_64) builds of Xray and sing-box plus geo files into engines/
# (bundled into the .deb / .rpm / AppImage as resources).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
source "$root/scripts/lib-fetch.sh"
out="$root/engines"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out"

fetch_asset XTLS/Xray-core '^Xray-linux-64\.zip$' "$tmp/xray.zip"
unzip -q -o "$tmp/xray.zip" -d "$tmp/xray"
install -m 755 "$tmp/xray/xray" "$out/xray"
cp "$tmp/xray/geoip.dat" "$tmp/xray/geosite.dat" "$out/"

fetch_asset SagerNet/sing-box '^sing-box-[0-9.]+-linux-amd64\.tar\.gz$' "$tmp/sb.tar.gz"
mkdir -p "$tmp/sb" && tar -xzf "$tmp/sb.tar.gz" -C "$tmp/sb"
install -m 755 "$(find "$tmp/sb" -type f -name sing-box | head -1)" "$out/sing-box"
cronet="$(find "$tmp/sb" -type f -name 'libcronet.so' | head -1)"
if [ -n "$cronet" ]; then cp "$cronet" "$out/"; fi

ls -la "$out"
