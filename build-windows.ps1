# Build Threadknot as an installable Windows package: the NSIS setup .exe, plus
# the headless server binary. Windows counterpart of build-linux.sh/build-mac.sh,
# and the same rule holds: only `tauri build` embeds the web UI, so never ship a
# plain `cargo build --release` binary.
#
# Usage: .\build-windows.ps1
#        .\build-windows.ps1 -Scratch    # force the separate target directory
#
# NOTE: keep this file ASCII-only. PowerShell 5.1 reads BOM-less files as ANSI
# and non-ASCII punctuation (smart quotes, long dashes) breaks parsing.
#
# WHY THIS SCRIPT EXISTS AT ALL
# -----------------------------
# Windows locks a running .exe, and cargo's final step is to REPLACE
# target\release\threadknot.exe. If the app is running from that exact path the
# build dies at the last moment with "failed to remove file ... Access is
# denied. (os error 5)", after having compiled everything.
#
# The intended arrangement (see scripts\restart-windows.ps1) is that the app
# runs from a stable copy at %LOCALAPPDATA%\Threadknot\threadknot.exe, leaving
# the build output free to overwrite. When that holds, this script is an
# ordinary `tauri build`.
#
# When it does not hold - the app was launched straight out of the checkout -
# stopping it is often not an option, because an agent session may be running
# THROUGH Threadknot and killing it cuts the very tool call doing the build. So
# this script detects that case and redirects the whole build to a separate
# target directory, which touches nothing the running process holds.
#
# That redirect is correct but expensive: a fresh target directory shares no
# cache with the normal one, so everything recompiles. Prefer fixing the
# arrangement (run the app from the LOCALAPPDATA copy) over paying that cost on
# every build.

[CmdletBinding()]
param([switch]$Scratch)

$ErrorActionPreference = 'Continue'
Set-Location (Split-Path -Parent $PSCommandPath)

function Fail($msg) { Write-Host "==> ERROR: $msg" -ForegroundColor Red; exit 1 }
function Say($msg)  { Write-Host "==> $msg" -ForegroundColor Cyan }

if (-not $IsWindows -and $env:OS -ne 'Windows_NT') {
  Fail "build-windows.ps1 only runs on Windows. Use build-linux.sh or build-mac.sh."
}
foreach ($tool in 'node','npm','cargo') {
  if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { Fail "$tool is not on PATH." }
}

$BuildExe = Join-Path (Get-Location) 'src-tauri\target\release\threadknot.exe'

# Is a running process holding the binary this build wants to replace? Compare
# resolved paths rather than process names: another Threadknot running from the
# LOCALAPPDATA copy is fine and must not trigger the expensive redirect.
$Blocking = @(Get-Process threadknot -ErrorAction SilentlyContinue | Where-Object {
  $_.Path -and ($_.Path -eq $BuildExe)
})

$TargetDir = $null
if ($Scratch -or $Blocking.Count -gt 0) {
  $TargetDir = Join-Path (Split-Path -Parent (Get-Location)) 'tk-release-target'
  if ($Blocking.Count -gt 0) {
    Say "threadknot.exe (PID $($Blocking[0].Id)) is running from the build output."
    Write-Host "    Windows locks it, so cargo could not replace it at the end of the build."
    Write-Host "    Redirecting this build to: $TargetDir"
    Write-Host "    This recompiles from scratch. To avoid it next time, run the app"
    Write-Host "    from the stable copy - see scripts\restart-windows.ps1."
  } else {
    Say "Building into the separate target directory: $TargetDir"
  }
  $env:CARGO_TARGET_DIR = $TargetDir
  $ReleaseDir = Join-Path $TargetDir 'release'
} else {
  $ReleaseDir = Join-Path (Get-Location) 'src-tauri\target\release'
}

Say "Building Threadknot NSIS installer - embeds UI via tauri build..."
npx tauri build --bundles nsis -- --locked
if ($LASTEXITCODE -ne 0) { Fail "tauri build failed." }

Say "Building headless server..."
cargo build --release --locked --bin threadknot-headless --manifest-path src-tauri/Cargo.toml
if ($LASTEXITCODE -ne 0) { Fail "headless build failed." }

# Same embed sanity-check as build-linux.sh/build-mac.sh: the fresh web bundle's
# hash must appear inside the shipped binary, or this is the broken dev build
# that dials localhost instead of serving the UI it was supposed to embed.
$Bin = Join-Path $ReleaseDir 'threadknot.exe'
$m = Select-String -Path 'dist\index.html' -Pattern 'assets/index-([^"]*)\.js'
if (-not $m) { Fail "no web bundle hash in dist\index.html - did the frontend build run?" }
$Hash = $m.Matches[0].Groups[1].Value
if (-not (Select-String -Path $Bin -Pattern $Hash -SimpleMatch -Encoding utf8 -Quiet -ErrorAction SilentlyContinue)) {
  Fail "web UI is NOT embedded in $Bin - this is the broken dev build."
}
Say "OK: web UI ($Hash) is embedded in $Bin"

Say "Done. Artifacts:"
Get-ChildItem (Join-Path $ReleaseDir 'bundle\nsis') -Filter *.exe -ErrorAction SilentlyContinue |
  ForEach-Object { Write-Host ("    " + $_.FullName) }
Write-Host ("    " + $Bin)
Write-Host ("    " + (Join-Path $ReleaseDir 'threadknot-headless.exe'))
