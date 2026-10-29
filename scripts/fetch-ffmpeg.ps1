# Downloads the pinned FFmpeg LGPL *shared* build for Windows x64 (BtbN) and lays it out in
# sala/third_party/ffmpeg/{bin,lib,include} (git-ignored). No GPL components (no libx264).
#
# Source : https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-07-31-14-10
# Version: FFmpeg n7.1.5 (ffmpeg-n7.1.5-12-g1fdbca85aa-win64-lgpl-shared-7.1) -> matches ffmpeg-next 7.1
# The cargo build finds it through FFMPEG_DIR set in sala/.cargo/config.toml (relative path).
# Also fetches libclang 18.1.1 (bindgen needs it to build ffmpeg-sys-next); see the end of the script.
#
# Usage: powershell -NoProfile -File scripts/fetch-ffmpeg.ps1 [-Force]
param([switch]$Force)
$ErrorActionPreference = 'Stop'

$tag = 'autobuild-2026-07-31-14-10'
$name = 'ffmpeg-n7.1.5-12-g1fdbca85aa-win64-lgpl-shared-7.1'
$url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$tag/$name.zip"

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'sala\third_party\ffmpeg'
$marker = Join-Path $dest 'VERSION.txt'

if ((Test-Path $marker) -and -not $Force) {
    if ((Get-Content $marker -Raw).Trim() -eq $name) {
        Write-Host "FFmpeg $name already in place at $dest"
        $haveFfmpeg = $true
    }
}

if (-not $haveFfmpeg) {
$tmp =Join-Path ([System.IO.Path]::GetTempPath()) ("concord-ffmpeg-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp "$name.zip"
    Write-Host "Downloading $url"
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $url -OutFile $zip
    Expand-Archive -Path $zip -DestinationPath $tmp
    $src = Join-Path $tmp $name
    if (-not (Test-Path (Join-Path $src 'bin\avcodec-61.dll'))) { throw "unexpected archive layout: avcodec-61.dll missing" }

    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
    New-Item -ItemType Directory -Path $dest | Out-Null
    foreach ($d in 'bin', 'lib', 'include') { Copy-Item -Recurse (Join-Path $src $d) (Join-Path $dest $d) }
    Copy-Item (Join-Path $src 'LICENSE.txt') $dest -ErrorAction SilentlyContinue
    Set-Content -Path $marker -Value $name
    Write-Host "FFmpeg $name installed at $dest"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
}

# libclang for bindgen (ffmpeg-sys-next). Pinned to 18.1.1 from the PyPI "libclang" wheel:
# bindgen 0.70 (used by ffmpeg-sys-next 7.1) emits broken layout asserts with a recent
# system LLVM (23.x), so LIBCLANG_PATH (sala/.cargo/config.toml) points at this copy.
$clangDir = Join-Path $root 'sala\third_party\libclang'
if (-not (Test-Path (Join-Path $clangDir 'libclang.dll')) -or $Force) {
    $wheelUrl = 'https://files.pythonhosted.org/packages/0b/2d/3f480b1e1d31eb3d6de5e3ef641954e5c67430d5ac93b7fa7e07589576c7/libclang-18.1.1-py2.py3-none-win_amd64.whl'
    $wheelSha = '4dd2d3b82fab35e2bf9ca717d7b63ac990a3519c7e312f19fa8e86dcc712f7fb'
    $tmp2 = Join-Path ([System.IO.Path]::GetTempPath()) ("concord-libclang-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp2 | Out-Null
    try {
        $wheel = Join-Path $tmp2 'libclang.zip'
        Invoke-WebRequest -Uri $wheelUrl -OutFile $wheel
        if ((Get-FileHash $wheel -Algorithm SHA256).Hash.ToLower() -ne $wheelSha) { throw 'libclang wheel hash mismatch' }
        Expand-Archive -Path $wheel -DestinationPath (Join-Path $tmp2 'x')
        $dll = Get-ChildItem -Recurse (Join-Path $tmp2 'x') -Filter libclang.dll | Select-Object -First 1
        New-Item -ItemType Directory -Force -Path $clangDir | Out-Null
        Copy-Item $dll.FullName $clangDir
        Write-Host "libclang 18.1.1 installed at $clangDir"
    } finally {
        Remove-Item -Recurse -Force $tmp2 -ErrorAction SilentlyContinue
    }
}
