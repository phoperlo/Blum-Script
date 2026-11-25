<#
.SYNOPSIS
    BlumScript installer for Windows.

.DESCRIPTION
    Installs the BlumScript compiler (blumsc), documentation, examples and the
    learning site into %LOCALAPPDATA%\BlumScript, and adds the bin directory
    to the current user's PATH.

    Works in two modes:
      1. From the project tree: builds target\release\blumsc.exe with cargo if needed.
      2. From a packaged dist: uses blumsc.exe located next to this script.

.PARAMETER Source
    Path to the blumscript_rust project (optional; auto-detected).

.PARAMETER NoPath
    Do not modify the user PATH.

.PARAMETER Build
    Force a cargo rebuild even if a binary already exists.

.PARAMETER Open
    Open the learning site after installation.

.EXAMPLE
    .\install.ps1

.EXAMPLE
    .\install.ps1 -Source "C:\src\blumscript_rust" -Build -Open
#>
param(
    [string]$Source = "",
    [switch]$NoPath,
    [switch]$Build,
    [switch]$Open
)

$ErrorActionPreference = "Stop"

function Write-Step([string]$msg) { Write-Host "  -> $msg" -ForegroundColor Cyan }
function Write-Ok([string]$msg)   { Write-Host "  OK $msg" -ForegroundColor Green }
function Fail([string]$msg)       { Write-Host "ERROR: $msg" -ForegroundColor Red; exit 1 }

Write-Host ""
Write-Host "BlumScript installer" -ForegroundColor Green
Write-Host "====================" -ForegroundColor Green
Write-Host ""

$scriptDir = $PSScriptRoot
if (-not $scriptDir) { $scriptDir = (Get-Location).Path }

# ---- locate assets and compiler binary ----
$prebuilt = Join-Path $scriptDir "blumsc.exe"
$project  = $null
if ($Source -ne "") {
    $project = (Resolve-Path $Source).Path
} else {
    $candidate = Split-Path $scriptDir -Parent
    if (Test-Path (Join-Path $candidate "Cargo.toml")) { $project = $candidate }
}

$exe = $null
$assetRoot = $null

if (Test-Path $prebuilt) {
    Write-Step "using packaged binary: $prebuilt"
    $exe = $prebuilt
    $assetRoot = $scriptDir
} else {
    if (-not $project) {
        Fail "cannot find the BlumScript project. Pass -Source <path to blumscript_rust>."
    }
    if (-not (Test-Path (Join-Path $project "Cargo.toml"))) {
        Fail "invalid project path (Cargo.toml not found): $project"
    }
    $assetRoot = $project
    $exe = Join-Path $project "target\release\blumsc.exe"

    if (-not (Test-Path $exe) -or $Build) {
        $cargo = Get-Command cargo -ErrorAction SilentlyContinue
        if (-not $cargo) {
            Fail "cargo not found in PATH. Install Rust (https://rustup.rs) or use a packaged build."
        }
        Write-Step "building BlumScript compiler (cargo build --release)..."
        Push-Location $project
        try {
            & cargo build --release
            if ($LASTEXITCODE -ne 0) { Fail "cargo build failed" }
        } finally {
            Pop-Location
        }
    }
    if (-not (Test-Path $exe)) {
        Fail "compiler binary not found after build: $exe"
    }
}

# ---- installation directory ----
$installDir = Join-Path $env:LOCALAPPDATA "BlumScript"
$binDir     = Join-Path $installDir "bin"
$docsDir    = Join-Path $installDir "docs"
$exampDir   = Join-Path $installDir "examples"
$siteDir    = Join-Path $installDir "site"

Write-Step "installing to $installDir"
New-Item -ItemType Directory -Force -Path $binDir, $docsDir, $exampDir, $siteDir | Out-Null

# ---- copy compiler ----
Copy-Item -Force $exe (Join-Path $binDir "blumsc.exe")
Write-Ok "compiler: $binDir\blumsc.exe"

# ---- copy docs ----
foreach ($doc in @("README.md", "DOCUMENTATION.md")) {
    $p = Join-Path $assetRoot $doc
    if (Test-Path $p) { Copy-Item -Force $p $docsDir }
}
Write-Ok "documentation -> $docsDir"

# ---- copy examples ----
$srcExamples = Join-Path $assetRoot "examples"
if (Test-Path $srcExamples) {
    Get-ChildItem -Path $srcExamples -Filter "*.blm" -File -ErrorAction SilentlyContinue |
        ForEach-Object { Copy-Item -Force $_.FullName $exampDir }
}
Write-Ok "examples -> $exampDir"

# ---- copy site ----
$srcSite = Join-Path $assetRoot "site"
if (Test-Path $srcSite) {
    Copy-Item -Recurse -Force (Join-Path $srcSite "*") $siteDir
}
Write-Ok "learning site -> $siteDir"

# ---- uninstaller ----
$uninstallSrc = Join-Path $scriptDir "uninstall.ps1"
if (Test-Path $uninstallSrc) {
    Copy-Item -Force $uninstallSrc (Join-Path $installDir "uninstall.ps1")
}

# ---- version file ----
@"
BlumScript
version: 0.2.0
installed: $(Get-Date -Format "yyyy-MM-dd HH:mm:ss")
compiler: $binDir\blumsc.exe
site: $siteDir\index.html
docs: $docsDir
"@ | Set-Content -Encoding UTF8 (Join-Path $installDir "version.txt")

# ---- add to user PATH ----
if (-not $NoPath) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($null -eq $userPath) { $userPath = "" }
    $parts = @()
    if ($userPath.Trim() -ne "") { $parts = $userPath.Split(';') | Where-Object { $_ -ne "" } }
    $already = $false
    foreach ($p in $parts) {
        if ($p.TrimEnd('\') -ieq $binDir.TrimEnd('\')) { $already = $true; break }
    }
    if (-not $already) {
        $newPath = ($parts + $binDir) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Ok "added to user PATH: $binDir"
        Write-Host "     (open a new terminal for PATH changes to take effect)" -ForegroundColor DarkGray
    } else {
        Write-Ok "PATH already contains $binDir"
    }
} else {
    Write-Step "skipped PATH modification (-NoPath)"
}

# ---- final message ----
Write-Host ""
Write-Host "BlumScript installed." -ForegroundColor Green
Write-Host ""
Write-Host "  Compiler : $binDir\blumsc.exe"
Write-Host "  Docs     : $docsDir"
Write-Host "  Examples : $exampDir"
Write-Host "  Site     : $siteDir\index.html"
Write-Host ""
Write-Host "Try:" -ForegroundColor Yellow
Write-Host "  blumsc run `"$exampDir\hello.blm`""
Write-Host "  blumsc build hello.blm -o hello.exe"
Write-Host ""
Write-Host "To uninstall:  powershell -ExecutionPolicy Bypass -File `"$installDir\uninstall.ps1`"" -ForegroundColor DarkGray
Write-Host ""

if ($Open) {
    $index = Join-Path $siteDir "index.html"
    if (Test-Path $index) { Start-Process $index }
}
