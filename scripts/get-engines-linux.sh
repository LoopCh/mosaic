#!/usr/bin/env bash
# Download the Linux (x86_64) builds of Xray and sing-box plus geo files into engines/
# (bundled into the .deb / .rpm / AppImage as resources).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/engines"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out"

auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $GITHUB_TOKEN"); fi
latest() { curl -fsSL "${auth[@]}" "https://api.github.com/repos/$1/releases/latest" | jq -r .tag_name; }

xver="$(latest XTLS/Xray-core)"
echo "Xray $xver"
curl -fsSL -o "$tmp/xray.zip" "https://github.com/XTLS/Xray-core/releases/download/$xver/Xray-linux-64.zip"
unzip -q -o "$tmp/xray.zip" -d "$tmp/xray"
install -m 755 "$tmp/xray/xray" "$out/xray"
cp "$tmp/xray/geoip.dat" "$tmp/xray/geosite.dat" "$out/"

sver="$(latest SagerNet/sing-box)"
sv="${sver#v}"
echo "sing-box $sver"
curl -fsSL -o "$tmp/sb.tar.gz" "https://github.com/SagerNet/sing-box/releases/download/$sver/sing-box-$sv-linux-amd64.tar.gz"
tar -xzf "$tmp/sb.tar.gz" -C "$tmp"
install -m 755 "$tmp/sing-box-$sv-linux-amd64/sing-box" "$out/sing-box"
if [ -f "$tmp/sing-box-$sv-linux-amd64/libcronet.so" ]; then cp "$tmp/sing-box-$sv-linux-amd64/libcronet.so" "$out/"; fi

ls -la "$out"
