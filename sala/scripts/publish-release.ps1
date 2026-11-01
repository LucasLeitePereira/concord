# Publishes a Concord release with the signed update manifest (D-038).
# Usage: .\scripts\publish-release.ps1 -Version 0.1.12 -Notes "What changed" [-DryRun] [-KeyPath <file>]
#
# Steps, stopping at the first error:
#   1. -Version matches installer\concord.iss and Cargo.toml, and the installer .exe exists
#      (build it first with scripts\build-installer.ps1).
#   2. The embedded PUBLIC_KEY is the real one (the ignored test fails while it is a placeholder).
#   3. Writes latest.json (UTF-8 without BOM) next to the installer.
#   4. Signs it with minisign (the password is asked by minisign in the terminal, never here).
#   5. Verifies the signature against concord-update.pub.
#   6. Unless -DryRun: one `gh release create` with the installer, latest.json and latest.json.minisig,
#      so `latest` never points to a release without its manifest.
# The private key (default %USERPROFILE%\.minisign\concord.key) never goes into the repository.
# Keep a backup of it: losing it means every friend must reinstall by hand once.
# Works in Windows PowerShell 5 and pwsh.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Notes = '',
    [switch]$DryRun,
    [string]$KeyPath = (Join-Path $env:USERPROFILE '.minisign\concord.key')
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$repo = 'LucasLeitePereira/concord'

function Fail([string]$message) {
    Write-Host "ERROR: $message" -ForegroundColor Red
    exit 1
}

function Step([string]$message) {
    Write-Host "==> $message"
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') { Fail "Version '$Version' must look like 1.2.3." }

# 1. Versions and installer
Step 'Checking versions and installer'
$iss = Join-Path $root 'installer\concord.iss'
$issMatch = Select-String -Path $iss -Pattern '#define AppVersion "([^"]+)"' | Select-Object -First 1
if (-not $issMatch) { Fail 'AppVersion not found in installer\concord.iss.' }
$issVersion = $issMatch.Matches[0].Groups[1].Value
$cargoMatch = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $cargoMatch) { Fail 'version not found in Cargo.toml.' }
$cargoVersion = $cargoMatch.Matches[0].Groups[1].Value
if ($issVersion -ne $Version) { Fail "concord.iss has $issVersion, expected $Version." }
if ($cargoVersion -ne $Version) { Fail "Cargo.toml has $cargoVersion, expected $Version." }
$outDir = Join-Path $root 'target\installer'
$setup = Join-Path $outDir "Concord-Setup-$Version.exe"
if (-not (Test-Path $setup)) { Fail "$setup not found. Run scripts\build-installer.ps1 first." }
$pub = Join-Path $root 'concord-update.pub'
if (-not (Test-Path $pub)) { Fail "$pub not found (the public key, versioned in the repository)." }
$minisign = Get-Command minisign -ErrorAction SilentlyContinue
if (-not $minisign) { Fail 'minisign not found in PATH (winget install jedisct1.minisign).' }

# 2. Real public key embedded in the app
Step 'Checking that PUBLIC_KEY is not the placeholder'
Push-Location $root
try {
    cargo test -p sala-core --lib update -- --ignored release_requires_real_public_key
    if ($LASTEXITCODE -ne 0) { Fail 'PUBLIC_KEY in update.rs is still the placeholder (or the test failed).' }
} finally {
    Pop-Location
}

# 3. Manifest
Step 'Writing latest.json'
$sha = (Get-FileHash -Algorithm SHA256 -Path $setup).Hash.ToLowerInvariant()
$url = "https://github.com/$repo/releases/download/v$Version/Concord-Setup-$Version.exe"
$manifest = [ordered]@{ version = $Version; url = $url; sha256 = $sha; notes = $Notes }
$json = $manifest | ConvertTo-Json
$manifestPath = Join-Path $outDir 'latest.json'
$sigPath = "$manifestPath.minisig"
if (Test-Path $sigPath) { Remove-Item $sigPath -Force }
[IO.File]::WriteAllText($manifestPath, $json, (New-Object System.Text.UTF8Encoding($false)))

# 4. Signature (minisign asks for the key password in the terminal)
Step 'Signing latest.json'
if (-not (Test-Path $KeyPath)) { Fail "Private key not found at $KeyPath (use -KeyPath)." }
& $minisign.Source -S -s $KeyPath -m $manifestPath
if ($LASTEXITCODE -ne 0) { Fail 'minisign -S failed.' }
if (-not (Test-Path $sigPath)) { Fail "$sigPath was not created." }

# 5. Verification
Step 'Verifying the signature with concord-update.pub'
& $minisign.Source -Vm $manifestPath -p $pub
if ($LASTEXITCODE -ne 0) { Fail 'The signature does not verify against concord-update.pub.' }

if ($DryRun) {
    Write-Host "DryRun: everything checks out; nothing was published. Would create v$Version with:" -ForegroundColor Yellow
    Write-Host "  $setup"
    Write-Host "  $manifestPath"
    Write-Host "  $sigPath"
    exit 0
}

# 6. Release: all three files in one command
Step "Creating release v$Version"
$token = (gh auth token -u LucasLeitePereira)
if ($LASTEXITCODE -ne 0 -or -not $token) { Fail 'gh has no token for LucasLeitePereira (gh auth login).' }
$previousToken = $env:GH_TOKEN
$env:GH_TOKEN = $token
try {
    $releaseNotes = if ($Notes) { $Notes } else { "Concord $Version" }
    gh release create "v$Version" $setup $manifestPath $sigPath --repo $repo --title "Concord $Version" --notes $releaseNotes
    if ($LASTEXITCODE -ne 0) { Fail 'gh release create failed.' }
} finally {
    $env:GH_TOKEN = $previousToken
}
Write-Host "OK: https://github.com/$repo/releases/tag/v$Version" -ForegroundColor Green
