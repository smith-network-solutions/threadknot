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

# Stage the verified bytes NOW, and promote the staged copy rather than $New.
#
# Verifying $New and then copying it ~30s later verifies nothing: the kill and
# port wait below take that long, and a `cargo build --release` finishing inside
# that window replaces $New with bytes nobody checked. The guard would pass and
# the broken binary would still be installed - the exact incident this script
# exists to prevent, just with a smaller window.
#
# So the hash that matters is taken from the staged file, which nothing else
# writes to, and that same file becomes $Live. Staged beside $Live rather than
# in the repo: same volume, so the promotion below is a rename, not a copy.
$LiveDir = Split-Path -Parent $Live
$null = New-Item -ItemType Directory -Force -Path $LiveDir -ErrorAction SilentlyContinue
$Staged = "$Live.staged"
try {
    Copy-Item $New $Staged -Force -ErrorAction Stop
} catch {
    Say "FAIL: could not stage the new binary: $($_.Exception.Message). Not restarting."
    Say '==================== done (FAIL) ===================='
    exit 1
}
if ((Get-FileHash -Path $Staged -Algorithm SHA256).Hash -ne $meta.sha256) {
    Say 'FAIL: the staged copy does not match the build stamp - the binary changed while this script was reading it (a build finishing underneath us?). Not restarting; the running app is untouched.'
    Remove-Item $Staged -Force -ErrorAction SilentlyContinue
    Say '==================== done (FAIL) ===================='
    exit 1
}
Say 'staged the verified binary'

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

# Promote the staged copy (retry briefly in case of trailing file lock).
# Move, not Copy: the bytes were hashed in place above, so renaming them into
# position is the only step that cannot introduce different ones.
$copied = $false
for ($i = 0; $i -lt 10; $i++) {
    try { Move-Item $Staged $Live -Force -ErrorAction Stop; $copied = $true; break }
    catch { Start-Sleep -Milliseconds 700 }
}
Say "binary promote: $(if ($copied) {'ok'} else {'FAILED, still locked?'})"
if (-not $copied) {
    # The old instance is already dead at this point, so exiting here would
    # leave the machine with NO app - the failure this script is meant to make
    # impossible. The previous $Live binary is still on disk and still good;
    # start it rather than stranding the user on nothing.
    Remove-Item $Staged -Force -ErrorAction SilentlyContinue
    if (Test-Path $Live) {
        Say 'relaunching the PREVIOUS binary so the machine is not left without one'
        Start-Process -FilePath $Live -WorkingDirectory $LiveDir
    } else {
        Say 'FAIL: no previous binary to fall back to'
    }
    Say '==================== done (FAIL) ===================='
    exit 1
}

# Carry the stamp across, so the installed copy can say what it is. Without
# this the live directory keeps whatever stamp it had, and an old one sitting
# beside a new exe is a file that lies about the binary next to it.
try {
    Copy-Item $Stamp (Join-Path $LiveDir 'threadknot.build.json') -Force -ErrorAction Stop
} catch {
    Say "note: could not copy the build stamp to the live dir ($($_.Exception.Message))"
}

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
