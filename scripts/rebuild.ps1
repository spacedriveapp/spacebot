# rebuild.ps1 — Rebuild and launch spacebot for development.
#
# Builds the backend (cargo), frontend (vite), and Tauri desktop app,
# then launches Spacebot.exe which automatically starts the backend
# server (with logs in the console) and opens devtools.
#
# Usage:
#   .\scripts\rebuild.ps1                   # full rebuild + launch
#   .\scripts\rebuild.ps1 -SkipFrontend     # backend only
#   .\scripts\rebuild.ps1 -SkipBackend      # frontend only
#   .\scripts\rebuild.ps1 -BuildOnly        # build without launching

param(
    [switch]$SkipFrontend,
    [switch]$SkipBackend,
    [switch]$BuildOnly
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
$InterfaceDir = Join-Path $RepoRoot "interface"
$DesktopDir = Join-Path $RepoRoot "desktop"
$BinariesDir = Join-Path $RepoRoot "desktop\src-tauri\binaries"
$BackendBin = Join-Path $RepoRoot "target\debug\spacebot.exe"
$TauriBin = Join-Path $RepoRoot "desktop\src-tauri\target\debug\Spacebot.exe"

function Log($msg)  { Write-Host "[rebuild] $msg" }
function Fail($msg) { Write-Host "[rebuild] ERROR: $msg" -ForegroundColor Red; exit 1 }

$sw = [System.Diagnostics.Stopwatch]::StartNew()

# ── Frontend rebuild ─────────────────────────────────────────────────────────
if (-not $SkipFrontend) {
    Log "rebuilding frontend..."

    if (-not (Get-Command bun -ErrorAction SilentlyContinue)) {
        Fail "bun is not installed. Install it from https://bun.sh"
    }

    $NodeModules = Join-Path $InterfaceDir "node_modules"
    if (-not (Test-Path $NodeModules)) {
        Log "installing frontend dependencies..."
        Push-Location $InterfaceDir
        try { bun install } finally { Pop-Location }
    }

    Push-Location $InterfaceDir
    try { bun run build } finally { Pop-Location }

    if ($LASTEXITCODE -ne 0) { Fail "frontend build failed" }
    Log "frontend ready -> $InterfaceDir\dist\"
} else {
    Log "skipping frontend"
}

# ── Backend rebuild ──────────────────────────────────────────────────────────
if (-not $SkipBackend) {
    Log "rebuilding backend..."

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail "cargo is not installed. Install Rust from https://rustup.rs"
    }

    $env:SPACEBOT_SKIP_FRONTEND_BUILD = "1"
    cargo build --manifest-path "$RepoRoot\Cargo.toml"

    if ($LASTEXITCODE -ne 0) { Fail "backend build failed" }
    Log "backend ready -> $BackendBin"
} else {
    Log "skipping backend"
}

# ── Bundle sidecar ───────────────────────────────────────────────────────────
if (-not $SkipBackend) {
    Log "bundling sidecar..."

    $HostTriple = (rustc -vV | Select-String "^host:").ToString().Split(" ")[1]
    if (-not (Test-Path $BinariesDir)) {
        New-Item -ItemType Directory -Path $BinariesDir -Force | Out-Null
    }

    $DestBin = Join-Path $BinariesDir "spacebot-${HostTriple}.exe"
    Copy-Item $BackendBin $DestBin -Force
    Log "sidecar bundled -> $DestBin"
}

# ── Tauri desktop rebuild ────────────────────────────────────────────────────
Log "rebuilding Tauri desktop app..."

# Kill any running instance so the linker can overwrite the exe
$ErrorActionPreference = "Continue"
taskkill /f /im Spacebot.exe 2>&1 | Out-Null
& $BackendBin stop 2>&1 | Out-Null
$ErrorActionPreference = "Stop"

cargo build --manifest-path "$RepoRoot\desktop\src-tauri\Cargo.toml"

if ($LASTEXITCODE -ne 0) { Fail "Tauri desktop build failed" }
Log "desktop ready -> $TauriBin"

# ── Build summary ────────────────────────────────────────────────────────────
$sw.Stop()
$elapsed = [math]::Round($sw.Elapsed.TotalSeconds)
Log "rebuild finished in ${elapsed}s"
if (-not $SkipFrontend) { Log "  frontend: $InterfaceDir\dist\" }
if (-not $SkipBackend)  { Log "  backend:  $BackendBin" }
Log "  desktop:  $TauriBin"

if ($BuildOnly) {
    Log "build-only mode, skipping launch"
    exit 0
}

# ══════════════════════════════════════════════════════════════════════════════
# LAUNCH
# ══════════════════════════════════════════════════════════════════════════════
# Spacebot.exe handles everything:
#   - Spawns the backend server and pipes logs to the console window
#   - Opens the Tauri frontend (webview)
#   - Opens Chrome DevTools (debug build)

Log "launching Spacebot.exe..."
Log "  console  -> backend server logs (localhost:19898)"
Log "  window   -> Tauri frontend app"
Log "  devtools -> opens automatically (debug build)"
Start-Process -FilePath $TauriBin
