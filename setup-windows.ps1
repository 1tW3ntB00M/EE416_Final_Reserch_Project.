#Requires -Version 5.1
<#
.SYNOPSIS
  One-shot idiot-proof setup for odin-gsp on Windows 10/11 (x64, MSVC).
  Run from PowerShell:  .\setup-windows.ps1
.DESCRIPTION
  Idempotent. Safe to re-run. Does, in order:
   1. Checks: git, cargo/rustc, cmake, MSVC Build Tools (via vswhere/registry)
   2. Ensures sibling ../odin-rs clone exists (required by [patch] in Cargo.toml)
   3. Auto-patches ../odin-rs/Cargo.toml to drop `bindgen` from gdal/gdal-sys
      (uses prebuilt 3_12 bindings; fresh MSVC bindgen mis-emits c_int vs c_uint)
   4. Ensures sibling ../vcpkg exists + bootstrapped, installs gdal:x64-windows if missing
   5. Detects installed GDAL version from include/gdal_version.h, syncs:
      - current session $env:GDAL_* + PATH (so cargo works NOW, no restart)
      - persistent HKCU via setx (future terminals) + removes hijacking GDAL_HOME
      - .cargo/config.toml [env] GDAL_VERSION (portable relative INCLUDE/LIB stay as-is)
   6. Optionally builds (cargo check). Use -SkipBuild to skip.
.EXAMPLE
  .\setup-windows.ps1
  .\setup-windows.ps1 -SkipBuild
#>
[CmdletBinding()]
param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"

function Fail([string]$msg) { Write-Error $msg; exit 1 }
function Info([string]$msg) { Write-Host "[setup] $msg" -ForegroundColor Cyan }
function Ok([string]$msg)   { Write-Host "[setup] $msg" -ForegroundColor Green }
function Warn([string]$msg) { Write-Host "[setup] WARN: $msg" -ForegroundColor Yellow }

$RepoRoot = $PSScriptRoot
$OdinRoot = Split-Path -Parent $RepoRoot
$OdinRs   = Join-Path $OdinRoot "odin-rs"
$Vcpkg    = Join-Path $OdinRoot "vcpkg"
$VcpkgExe = Join-Path $Vcpkg "vcpkg.exe"
$TripletDir = Join-Path $Vcpkg "installed\x64-windows"
$GdalInclude = Join-Path $TripletDir "include"
$GdalLib     = Join-Path $TripletDir "lib"
$GdalBin     = Join-Path $TripletDir "bin"
$GdalHeader  = Join-Path $GdalInclude "gdal_version.h"
$CargoConfig = Join-Path $RepoRoot ".cargo\config.toml"

# --- 0. OS guard ---
if (-not ([System.Environment]::OSVersion.Platform -eq "Win32NT")) { Fail "Windows-only script." }
if (-not [System.Environment]::Is64BitOperatingSystem) { Fail "Requires 64-bit Windows." }

# --- 1. Prereq checks ---
Info "Checking prerequisites..."
foreach ($cmd in @("git", "cargo", "rustc", "cmake")) {
  if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
    Fail "Missing '$cmd'. Install: git https://git-scm.com/download/win | Rust https://rustup.rs (MSVC) | cmake https://cmake.org/download/"
  }
}
# MSVC check via vswhere, fallback to registry, fallback to cl.exe on PATH
$VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$HasMsvc = $false
if (Test-Path $VsWhere) {
  $vs = & $VsWhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null | Select-Object -First 1
  if ($vs) { $HasMsvc = $true }
}
if (-not $HasMsvc) {
  if (Get-Command cl -ErrorAction SilentlyContinue) { $HasMsvc = $true }
  elseif (Test-Path "HKLM:\SOFTWARE\Microsoft\VisualStudio") { $HasMsvc = $true }
}
if (-not $HasMsvc) {
  Fail "MSVC Build Tools not found. Install 'Desktop development with C++' from https://visualstudio.microsoft.com/downloads/ (free Build Tools), then re-run."
}
Ok "git/cargo/rustc/cmake + MSVC present."

# --- 2. Sibling odin-rs ---
if (-not (Test-Path (Join-Path $OdinRs "Cargo.toml"))) {
  Info "Cloning odin-rs sibling into $OdinRs ..."
  git clone https://github.com/ODIN-fire/odin-rs $OdinRs
  if ($?) { } else { Fail "git clone odin-rs failed." }
} else {
  Ok "odin-rs sibling present."
}

# --- 3. Auto-patch odin-rs bindgen ---
$RsManifest = Join-Path $OdinRs "Cargo.toml"
$txt = Get-Content -Raw -LiteralPath $RsManifest
$orig = $txt
# gdal with array+bindgen -> array only
$txt = $txt -replace 'gdal\s*=\s*\{\s*version\s*=\s*"0\.19"\s*,\s*features\s*=\s*\["array"\s*,\s*"bindgen"\s*\]\s*\}', 'gdal = { version = "0.19", features = ["array"] }'
# gdal-sys with bindgen -> no bindgen features
$txt = $txt -replace 'gdal-sys\s*=\s*\{\s*version\s*=\s*"0\.12"\s*,\s*features\s*=\s*\["bindgen"\s*\]\s*\}', 'gdal-sys = { version = "0.12" }'
if ($txt -ne $orig) {
  Copy-Item -LiteralPath $RsManifest -Destination ($RsManifest + ".bak-bindgen") -Force
  Set-Content -LiteralPath $RsManifest -Value $txt -NoNewline
  Ok "Patched odin-rs/Cargo.toml (removed bindgen). Backup: Cargo.toml.bak-bindgen"
} else {
  Ok "odin-rs gdal deps already bindgen-free."
}

# --- 4. vcpkg + gdal ---
if (-not (Test-Path $VcpkgExe)) {
  if (-not (Test-Path (Join-Path $Vcpkg ".git"))) {
    Info "Cloning vcpkg into $Vcpkg ..."
    git clone https://github.com/microsoft/vcpkg $Vcpkg
  }
  Info "Bootstrapping vcpkg (one-time, a few minutes)..."
  & (Join-Path $Vcpkg "bootstrap-vcpkg.bat")
}
$needGdal = (-not (Test-Path $GdalHeader)) -or (-not (Test-Path (Join-Path $GdalLib "gdal.lib"))) -or (-not (Test-Path (Join-Path $GdalBin "gdal.dll")))
if ($needGdal) {
  Info "Installing gdal:x64-windows (one-time, 20-60 min)..."
  & $VcpkgExe install gdal:x64-windows
  if (-not (Test-Path $GdalHeader)) { Fail "gdal install finished but $GdalHeader missing." }
} else {
  Ok "vcpkg gdal:x64-windows already installed."
}

# --- 5. Detect GDAL version + wire env ---
$ver = "3.12.4"
if (Test-Path $GdalHeader) {
  $m = Select-String -LiteralPath $GdalHeader -Pattern '#\s*define\s+GDAL_RELEASE_NAME\s+"([^"]+)"' | Select-Object -First 1
  if ($m -and $m.Matches.Groups[1].Value) { $ver = $m.Matches.Groups[1].Value }
}
Info "GDAL version: $ver"

# session env (works NOW, no restart)
$env:GDAL_VERSION = $ver
$env:GDAL_INCLUDE_DIR = $GdalInclude
$env:GDAL_LIB_DIR = $GdalLib
if ($env:PATH -notlike "*$GdalBin*") { $env:PATH = "$GdalBin;$env:PATH" }

# persistent env (future terminals) — setx needs no admin, targets HKCU
foreach ($pair in @(@("GDAL_VERSION", $ver), @("GDAL_INCLUDE_DIR", $GdalInclude), @("GDAL_LIB_DIR", $GdalLib))) {
  & setx $pair[0] $pair[1] | Out-Null
}
Remove-ItemProperty -Path HKCU:\Environment -Name GDAL_HOME -ErrorAction SilentlyContinue
Ok "Session + persistent GDAL env set (GDAL_HOME hijack removed)."

# sync .cargo/config.toml GDAL_VERSION (INCLUDE/LIB stay portable relative paths)
if (Test-Path $CargoConfig) {
  $cfg = Get-Content -Raw -LiteralPath $CargoConfig
  if ($cfg -match 'GDAL_VERSION\s*=') {
    $cfg = $cfg -replace 'GDAL_VERSION\s*=\s*"[^"]*"', ('GDAL_VERSION = "{0}"' -f $ver)
  } else {
    $cfg += "`r`n[env]`r`nGDAL_VERSION = `"$ver`"`r`n"
  }
  Set-Content -LiteralPath $CargoConfig -Value $cfg -NoNewline
  Ok ".cargo/config.toml GDAL_VERSION synced to $ver."
} else {
  Warn ".cargo/config.toml not found, skipping sync."
}

# --- 6. Build sanity ---
if (-not $SkipBuild) {
  Info "Running cargo check (first build downloads crates + compiles, few minutes)..."
  Push-Location $RepoRoot
  try {
    # stale gdal-sys fingerprint from before env fix causes bogus errors; clean those two pkgs only
    cargo clean -p gdal-sys -p gdal 2>$null | Out-Null
    cargo check 2>&1 | Select-Object -Last 30
    Ok "cargo check done. Run with: cargo run (DLL already on this session PATH)."
  } finally { Pop-Location }
} else {
  Info "Skipped build. Next: cd odin-gsp; `$env:PATH=`"$GdalBin;`$env:PATH`"; cargo run"
}

Ok "DONE. Every new terminal needs the DLL on PATH once: `$env:PATH=`"$GdalBin;`$env:PATH`" (VSCode terminals get it auto via .vscode/settings.json)."
