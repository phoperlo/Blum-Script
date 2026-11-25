# Builds a distributable BlumScript package (dist/ + zip).
param(
    [switch]$NoZip
)

$ErrorActionPreference = "Stop"

function Step([string]$m) { Write-Host "  -> $m" -ForegroundColor Cyan }
function Ok([string]$m)   { Write-Host "  OK $m" -ForegroundColor Green }
function Fail([string]$m) { Write-Host "ERROR: $m" -ForegroundColor Red; exit 1 }

$installerDir = $PSScriptRoot
$project = Split-Path $installerDir -Parent
if (-not (Test-Path (Join-Path $project "Cargo.toml"))) {
    Fail "Cargo.toml not found in project root: $project"
}

Write-Host ""
Write-Host "BlumScript package builder" -ForegroundColor Green
Write-Host "==========================" -ForegroundColor Green
Write-Host ""

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) { Fail "cargo not found in PATH" }

Step "cargo build --release"
Push-Location $project
try {
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { Fail "cargo build failed" }
} finally { Pop-Location }

$exe = Join-Path $project "target\release\blumsc.exe"
if (-not (Test-Path $exe)) { Fail "compiler binary not found: $exe" }

$dist = Join-Path $project "dist"
Step "preparing $dist"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force -Path $dist, (Join-Path $dist "examples"), (Join-Path $dist "site") | Out-Null

Copy-Item -Force $exe (Join-Path $dist "blumsc.exe")
Ok "blumsc.exe"

foreach ($doc in @("README.md", "DOCUMENTATION.md")) {
    $p = Join-Path $project $doc
    if (Test-Path $p) { Copy-Item -Force $p $dist; }
}
Ok "documentation"

Get-ChildItem -Path (Join-Path $project "examples") -Filter "*.blm" -File |
    ForEach-Object { Copy-Item -Force $_.FullName (Join-Path $dist "examples") }
Ok "examples"

$site = Join-Path $project "site\index.html"
if (Test-Path $site) { Copy-Item -Force $site (Join-Path $dist "site") }
Ok "site"

foreach ($f in @("install.ps1", "install.bat", "uninstall.ps1")) {
    $p = Join-Path $installerDir $f
    if (Test-Path $p) { Copy-Item -Force $p $dist }
}
Ok "installer scripts"

if (-not $NoZip) {
    $zip = Join-Path $project "blumscript-1.0.0-windows.zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Step "compressing $zip"
    Compress-Archive -Path (Join-Path $dist "*") -DestinationPath $zip
    Ok "zip: $zip"
}

Write-Host ""
Write-Host "Package ready: $dist" -ForegroundColor Green
Write-Host "Run $(Join-Path $dist 'install.bat') to install on Windows." -ForegroundColor Yellow
Write-Host ""
