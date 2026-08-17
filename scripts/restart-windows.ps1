# Windows equivalent of scripts/restart.sh - safely swap the running Threadknot app
# onto a freshly built release binary, fully detached from the caller.
#
# WHY: the agent session may be running THROUGH Threadknot. If it kills Threadknot from
# its own shell, the tool call can die mid-restart. This script is launched via
# Start-Process (own process, not a child of the app), so the kill-copy-relaunch
# finishes even if the caller is interrupted.
#
# HOW AGENTS SHOULD INVOKE (fire-and-forget, detached):
#   Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','<path-to-checkout>\scripts\restart-windows.ps1'
# Then wait ~15s and verify from the log (do NOT relaunch on your own):
#   Get-Content $env:TEMP\threadknot-restart.log
#   Get-NetTCPConnection -LocalPort 42800 -State Listen
#
# ONE launch attempt, then verify only. Never a relaunch loop.
# NOTE: keep this file ASCII-only. PowerShell 5.1 reads BOM-less files as ANSI
# and non-ASCII punctuation (smart quotes, long dashes) breaks parsing.

$ErrorActionPreference = 'Continue'
$Log  = Join-Path $env:TEMP 'threadknot-restart.log'
# Derived from this script's own location, never hardcoded - every checkout
# lives somewhere different, and a baked-in path restarts nothing (or somebody
# else's binary).
$Repo = Split-Path -Parent $PSScriptRoot
$New  = Join-Path $Repo 'src-tauri\target\release\threadknot.exe'
# Windows locks a running .exe, so the app runs from a stable copy that the
# freshly built binary can overwrite. Override with THREADKNOT_LIVE_EXE.
$Live = $env:THREADKNOT_LIVE_EXE
if (-not $Live) { $Live = Join-Path $env:LOCALAPPDATA 'Threadknot\threadknot.exe' }
$Port = 42800

function Say($m) { "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')  $m" | Add-Content $Log }

Say '==================== restart begin ===================='

# Refuse to restart onto a binary that `tauri build` did not produce.
#
# This script used to copy $New over $Live with no check whatsoever, and then
# "verify" the result by watching for a listener on 42800 - which a broken build
# provides just as happily as a good one. On 2026-08-16 that is exactly what
# happened: a `cargo build --release` binary was installed, the log said "OK:
# threadknot is back up", and every window it opened showed
# ERR_CONNECTION_REFUSED because the webview was pointed at devUrl
# (localhost:1430) while the Rust server behind it ran fine.
#
# rebuild.sh stamps threadknot.build.json beside the exe with the SHA256 of what
# it built; a later cargo build overwrites the exe and leaves the stamp, so the
# hash stops matching. Checked BEFORE the running instance is stopped, so a bad
# build costs nothing - the app the user has keeps running untouched.
$Stamp = Join-Path (Split-Path -Parent $New) 'threadknot.build.json'
if (-not (Test-Path $New)) {
    Say "FAIL: no binary at $New - nothing to restart onto"
    Say '==================== done (FAIL) ===================='
    exit 1
}
if (-not (Test-Path $Stamp)) {
    Say 'FAIL: no threadknot.build.json beside the new binary - build it with ./rebuild.sh. Not restarting; the running app is untouched.'
    Say '==================== done (FAIL) ===================='
    exit 1
}
try {
    $meta = Get-Content $Stamp -Raw -ErrorAction Stop | ConvertFrom-Json
} catch {
    Say "FAIL: threadknot.build.json is unreadable: $($_.Exception.Message). Not restarting."
    Say '==================== done (FAIL) ===================='
    exit 1
}
# -ne on strings is case-insensitive here, which is what we want: sha256sum
# writes lower case, Get-FileHash returns upper.
if ($meta.kind -ne 'tauri-build' -or (Get-FileHash -Path $New -Algorithm SHA256).Hash -ne $meta.sha256) {
    Say 'FAIL: the new binary does not match its build stamp - it was rebuilt by something other than ./rebuild.sh (most likely cargo build --release). Not restarting; the running app is untouched.'
    Say '==================== done (FAIL) ===================='
    exit 1
}
Say "verified: tauri build stamp matches (commit $($meta.commit), ui $($meta.assetHash), built $($meta.builtAt))"

# Let the launching shell return first (it may be a child of threadknot).
Start-Sleep -Seconds 3

# Identify the live instance by the port it owns, fallback to process name.
$conn = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
$oldPid = if ($conn) { $conn[0].OwningProcess } else { (Get-Process threadknot -ErrorAction SilentlyContinue | Select-Object -First 1).Id }
Say "old pid: $oldPid"

# Graceful close, then force.
if ($oldPid) {
    $proc = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
    if ($proc) {
        $null = $proc.CloseMainWindow()
        if (-not $proc.WaitForExit(12000)) {
            Say 'did not exit gracefully; forcing'
            Stop-Process -Id $oldPid -Force -ErrorAction SilentlyContinue
        }
        Say 'old instance stopped'
    }
}

# Wait for the port to free up.
for ($i = 0; $i -lt 20; $i++) {
    if (-not (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue)) { break }
    Start-Sleep -Milliseconds 500
}

# Swap the binary (retry briefly in case of trailing file lock).
$null = New-Item -ItemType Directory -Force -Path (Split-Path $Live) -ErrorAction SilentlyContinue
$copied = $false
for ($i = 0; $i -lt 10; $i++) {
    try { Copy-Item $New $Live -Force -ErrorAction Stop; $copied = $true; break }
    catch { Start-Sleep -Milliseconds 700 }
}
Say "binary copy: $(if ($copied) {'ok'} else {'FAILED, still locked?'})"
if (-not $copied) { Say '==================== done (FAIL) ===================='; exit 1 }

# Ship the web bundle alongside the binary. The server resolves the LAN/phone
# UI from a dist folder near the exe or the working directory (resolve_dist in
# server.rs); the live copy runs far from the checkout, so without this copy
# every launch of it serves the "Web UI not built yet" fallback page.
$DistSrc = Join-Path $Repo 'dist'
$DistDst = Join-Path (Split-Path $Live) 'dist'
if (Test-Path (Join-Path $DistSrc 'index.html')) {
    try {
        if (Test-Path $DistDst) { Remove-Item $DistDst -Recurse -Force -ErrorAction Stop }
        Copy-Item $DistSrc $DistDst -Recurse -Force -ErrorAction Stop
        Say 'web dist copy: ok'
    } catch { Say "web dist copy: FAILED ($($_.Exception.Message))" }
} else {
    Say 'web dist copy: skipped (no dist/index.html in checkout)'
}

# ONE launch attempt.
Start-Process -FilePath $Live -WorkingDirectory (Split-Path $Live)
Say 'launched new binary'

# Verify it comes back up. No relaunch loop.
$up = $false
for ($i = 0; $i -lt 60; $i++) {
    if (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue) { $up = $true; break }
    Start-Sleep -Milliseconds 500
}
Say $(if ($up) { "OK: threadknot is back up, $Port listening" } else { "FAIL: $Port not listening after 30s, NOT relaunching" })
Say '==================== done ===================='
