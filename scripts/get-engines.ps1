# Downloads the latest Xray-core and sing-box for Windows x64 into .\engines
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\get-engines.ps1
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'engines'
$tmp  = Join-Path ([IO.Path]::GetTempPath()) ('mosaic-engines-' + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $dest, $tmp | Out-Null

$headers = @{ 'User-Agent' = 'mosaic-setup' }
if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }

function Get-LatestAsset([string]$repo, [string]$pattern) {
    $rel = Invoke-RestMethod -MaximumRetryCount 5 -RetryIntervalSec 5 -Headers $headers -Uri "https://api.github.com/repos/$repo/releases/latest"
    $asset = $rel.assets | Where-Object { $_.name -match $pattern } | Select-Object -First 1
    if (-not $asset) { throw "No asset matching '$pattern' in $repo $($rel.tag_name)" }
    Write-Host "  $repo $($rel.tag_name) -> $($asset.name)"
    $zip = Join-Path $tmp $asset.name
    Invoke-WebRequest -MaximumRetryCount 5 -RetryIntervalSec 5 -Headers $headers -Uri $asset.browser_download_url -OutFile $zip
    return $zip
}

Write-Host 'Xray-core:'
$zip = Get-LatestAsset 'XTLS/Xray-core' '^Xray-windows-64\.zip$'
$x = Join-Path $tmp 'xray'
Expand-Archive -Path $zip -DestinationPath $x -Force
Copy-Item (Join-Path $x 'xray.exe') $dest -Force
foreach ($f in 'geoip.dat', 'geosite.dat') {
    $p = Join-Path $x $f
    if (Test-Path $p) { Copy-Item $p $dest -Force }
}

Write-Host 'sing-box:'
$zip = Get-LatestAsset 'SagerNet/sing-box' '^sing-box-[0-9.]+-windows-amd64\.zip$'
$s = Join-Path $tmp 'sing-box'
Expand-Archive -Path $zip -DestinationPath $s -Force
$exe = Get-ChildItem -Path $s -Recurse -Filter 'sing-box.exe' | Select-Object -First 1
Copy-Item $exe.FullName $dest -Force

# Wintun: sing-box normally embeds it; keep a copy next to it just in case.
try {
    $wz = Join-Path $tmp 'wintun.zip'
    Invoke-WebRequest -MaximumRetryCount 5 -RetryIntervalSec 5 -Uri 'https://www.wintun.net/builds/wintun-0.14.1.zip' -OutFile $wz
    $w = Join-Path $tmp 'wintun'
    Expand-Archive -Path $wz -DestinationPath $w -Force
    Copy-Item (Join-Path $w 'wintun\bin\amd64\wintun.dll') $dest -Force
    Write-Host 'wintun.dll: ok'
} catch {
    Write-Warning "wintun.dll was not downloaded ($_). Only matters if TUN mode fails."
}

Remove-Item -Recurse -Force $tmp
Write-Host ''
Write-Host "Engines installed to $dest"
& (Join-Path $dest 'xray.exe') version | Select-Object -First 1
$sbv = & (Join-Path $dest 'sing-box.exe') version | Select-Object -First 1
Write-Host $sbv
if ($sbv -match 'version 1\.(\d+)\.' -and [int]$Matches[1] -lt 12) {
    Write-Warning 'Mosaic needs sing-box 1.12 or newer.'
}
