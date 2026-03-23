# bundle-sidecar.ps1 — Build the spacebot binary and copy it into the
# Tauri sidecar binaries directory with the correct target-triple suffix.
#
# Usage:
#   .\scripts\bundle-sidecar.ps1 [-Release]
#
# Tauri expects sidecar binaries at:
#   desktop\src-tauri\binaries\spacebot-<target-triple>.exe

param(
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$BinariesDir = Join-Path $RepoRoot "desktop\src-tauri\binaries"

# Determine Rust target triple
$HostTriple = (rustc -vV | Select-String "^host:").ToString().Split(" ")[1]
$TargetTriple = if ($env:TAURI_ENV_TARGET_TRIPLE) { $env:TAURI_ENV_TARGET_TRIPLE } else { $HostTriple }

# Build mode
if ($Release) {
    $BuildMode = "release"
    $CargoFlags = @("--release")
} else {
    $BuildMode = "debug"
    $CargoFlags = @()
}

Write-Host "Building spacebot ($BuildMode) for $TargetTriple..."

if ($TargetTriple -ne $HostTriple) {
    cargo build @CargoFlags --target $TargetTriple --manifest-path "$RepoRoot\Cargo.toml"
    $SrcBin = Join-Path $RepoRoot "target\$TargetTriple\$BuildMode\spacebot.exe"
} else {
    cargo build @CargoFlags --manifest-path "$RepoRoot\Cargo.toml"
    $SrcBin = Join-Path $RepoRoot "target\$BuildMode\spacebot.exe"
}

# Destination with target triple suffix (Tauri convention)
if (-not (Test-Path $BinariesDir)) {
    New-Item -ItemType Directory -Path $BinariesDir -Force | Out-Null
}

$DestBin = Join-Path $BinariesDir "spacebot-${TargetTriple}.exe"

Copy-Item $SrcBin $DestBin -Force
Write-Host "Copied $SrcBin -> $DestBin"
Write-Host "Sidecar binary ready."
