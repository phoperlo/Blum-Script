# BlumScript uninstaller
# Removes the installation directory and the bin entry from the user PATH.
param(
    [switch]$NoPath
)

$ErrorActionPreference = "SilentlyContinue"

$installDir = $PSScriptRoot
if (-not $installDir) { $installDir = Join-Path $env:LOCALAPPDATA "BlumScript" }
$binDir = Join-Path $installDir "bin"

Write-Host ""
Write-Host "BlumScript uninstaller" -ForegroundColor Yellow
Write-Host "======================" -ForegroundColor Yellow
Write-Host ""

if (-not $NoPath) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath) {
        $parts = $userPath.Split(';') | Where-Object { $_ -ne "" }
        $kept = @()
        $removed = $false
        foreach ($p in $parts) {
            if ($p.TrimEnd('\') -ieq $binDir.TrimEnd('\')) { $removed = $true }
            else { $kept += $p }
        }
        if ($removed) {
            [Environment]::SetEnvironmentVariable("Path", ($kept -join ';'), "User")
            Write-Host "  removed from user PATH: $binDir" -ForegroundColor Green
        } else {
            Write-Host "  PATH does not contain: $binDir" -ForegroundColor DarkGray
        }
    }
} else {
    Write-Host "  skipped PATH modification (-NoPath)" -ForegroundColor DarkGray
}

Write-Host "  removing $installDir ..." -ForegroundColor Cyan
Set-Location $env:TEMP
Remove-Item -Recurse -Force $installDir

Write-Host ""
Write-Host "BlumScript removed." -ForegroundColor Green
Write-Host ""
