# Builds the workspace in release mode and compiles the Concord installer.
# Output: sala\target\installer\Concord-Setup-<versão>.exe
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$candidates = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
)
$iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Error "ISCC.exe not found. Install: winget install --id JRSoftware.InnoSetup -e"
    exit 1
}

Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { Write-Error "cargo build --release failed"; exit 1 }
    & $iscc (Join-Path $root 'installer\concord.iss')
    if ($LASTEXITCODE -ne 0) { Write-Error "ISCC failed"; exit 1 }
    $ver = (Select-String -Path (Join-Path $root 'installer\concord.iss') -Pattern '#define AppVersion "([^"]+)"').Matches[0].Groups[1].Value
    $out = Join-Path $root "target\installer\Concord-Setup-$ver.exe"
    $size = [math]::Round((Get-Item $out).Length / 1MB, 2)
    Write-Host "OK: $out ($size MB)"
} finally {
    Pop-Location
}
