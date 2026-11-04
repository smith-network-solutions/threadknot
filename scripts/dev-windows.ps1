# Windows dev launcher: run Threadknot with fast reload, fully detached.
#
# WHY THIS EXISTS: a built app's window loads the frontend bundle embedded in
# the binary (WebviewUrl::App in lib.rs), so a UI change only shows after
# `npm run build` + a relaunch. Only `tauri dev` points the window at Vite on
# :1430, where Fast Refresh applies an edit in about a second with the app's
# state intact.
#
# WHY DETACHED: a dev stack started from an agent's shell dies with that
# shell - the session ends, the job object is torn down, and the window
# vanishes "cleanly" for no visible reason. The dev stack is therefore spawned
# through Win32_Process.Create, which parents it to the WMI host instead of to
# the caller, so it keeps running after the caller is gone. Same reasoning as
# restart-windows.ps1, different job.
#
# USAGE
#   scripts\dev.cmd      (wrapper: any shell, any directory, no policy flags)
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\dev-windows.ps1
#   ... -Takeover        close the installed app and take :42800 + real data
#   ... -Stop            stop the dev stack (leaves the installed app alone)
#   ... -Fresh           force the sandboxed profile even if :42800 is free
#   ... -Port 42815      pick the server port yourself
#
# WHICH PROFILE IT PICKS, and why it is automatic: two Threadknot servers
# cannot share :42800, and two WebView2 environments cannot share a user-data
# folder when their browser arguments differ (that is the 0x8007139F this
# config's additionalBrowserArgs would otherwise cause). So:
#   installed app NOT running -> port 42800 and your real ~/.threadknot:
#                                the dev window is your actual app.
#   installed app running     -> port 42810 with a scratch data dir and its
#                                own WebView2 folder, side by side with it.
#                                That window starts as a fresh install.
#
# NOTE: keep this file ASCII-only. PowerShell 5.1 reads BOM-less files as ANSI
# and non-ASCII punctuation breaks parsing.

param(
  [int]$Port = 0,
  [string]$DataDir = '',
  [switch]$Fresh,
  [switch]$Takeover,
  [switch]$Stop
)

$ErrorActionPreference = 'Continue'
$Repo = Split-Path -Parent $PSScriptRoot
$Log = Join-Path $env:TEMP 'threadknot-dev.log'
$VitePort = 1430   # must match vite.config.ts and tauri.conf.json's devUrl

# The console is the primary channel here: while the previous dev stack is
# still shutting down it holds the log file open, and a failed write must not
# look like a failed launch.
function Say($m) {
  $line = "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')  $m"
  $line | Add-Content $Log -ErrorAction SilentlyContinue
  Write-Output $line
}

function Get-ListenerPids([int]$p) {
  try {
    @(Get-NetTCPConnection -LocalPort $p -State Listen -ErrorAction Stop |
      Select-Object -ExpandProperty OwningProcess -Unique)
  } catch { @() }
}

# Everything this script started last time: the Vite server, the tauri CLI
# watching it, and the debug binary itself. The installed app is matched by
# none of these - it runs from %LOCALAPPDATA% and never from target\debug.
#
# Matched on ExecutablePath as well as command line, deliberately: `tauri dev`
# launches the binary as `target\debug\threadknot.exe`, a RELATIVE path, so a
# command-line match against the repo root misses the one process that matters
# most. When it is missed the symptom is quiet - the old instance keeps the
# port, the new one starts anyway, and its server never binds.
function Stop-DevStack {
  $targets = @()
  $targets += Get-ListenerPids $VitePort
  $escaped = [regex]::Escape($Repo)
  $targets += @(Get-CimInstance Win32_Process -Filter "name='node.exe' or name='threadknot.exe' or name='cmd.exe'" -ErrorAction SilentlyContinue |
    Where-Object {
      ($_.ExecutablePath -and $_.ExecutablePath.StartsWith($Repo, [StringComparison]::OrdinalIgnoreCase)) -or
      ($_.CommandLine -and $_.CommandLine -match $escaped -and $_.CommandLine -match 'tauri|vite')
    } |
    Select-Object -ExpandProperty ProcessId)
  $stopped = 0
  foreach ($id in ($targets | Select-Object -Unique)) {
    try { Stop-Process -Id $id -Force -ErrorAction Stop; $stopped++ } catch { }
  }
  Say "stopped $stopped dev process(es)"
}

Say '==================== dev launcher ===================='

if ($Stop) {
  Stop-DevStack
  return
}

# One stack at a time: a second Vite cannot take :1430 (strictPort), and the
# failure it produces - tauri loading the OLD server - looks like fast reload
# silently not working.
Stop-DevStack
Start-Sleep -Seconds 1

# The installed app: a threadknot.exe that is not this checkout's debug build.
$installed = @(Get-CimInstance Win32_Process -Filter "name='threadknot.exe'" -ErrorAction SilentlyContinue |
  Where-Object { -not ($_.ExecutablePath -and $_.ExecutablePath.StartsWith($Repo, [StringComparison]::OrdinalIgnoreCase)) })
$installedUp = @(Get-ListenerPids 42800).Count -gt 0

# -Takeover hands :42800 and the real data directory to the dev build, which
# means closing the installed app first. The kill is deliberately NOT done
# here: an agent session usually runs THROUGH that app, so killing it from
# this process would cut the caller off mid-command and leave nothing to
# start the replacement. It is folded into the detached command below, which
# belongs to the WMI host and finishes regardless.
if ($Takeover -and $installed.Count -gt 0) {
  $installedUp = $false
  Say "takeover: the installed app (pid $($installed.ProcessId -join ', ')) will be closed by the detached launcher"
}

$sandboxed = $Fresh -or $installedUp

if ($Port -eq 0) { $Port = if ($sandboxed) { 42810 } else { 42800 } }

$envPairs = @("THREADKNOT_PORT=$Port")
if ($DataDir) {
  $envPairs += "THREADKNOT_DATA_DIR=$DataDir"
} elseif ($sandboxed) {
  $scratch = Join-Path $env:TEMP 'tk-dev-data'
  $webview = Join-Path $env:TEMP 'tk-dev-webview'
  New-Item -ItemType Directory -Force -Path $scratch, $webview | Out-Null
  $envPairs += "THREADKNOT_DATA_DIR=$scratch"
  # Its own WebView2 environment, or creating the webview fails with
  # 0x8007139F while the installed app holds the shared one.
  $envPairs += "WEBVIEW2_USER_DATA_FOLDER=$webview"
}

if ($sandboxed) {
  Say "installed app is on :42800 - starting side by side on :$Port with a scratch profile"
  Say 'that window opens as a fresh install (pick your skin in Settings)'
} else {
  Say "starting on :$Port with your real data directory"
}

Remove-Item $Log -ErrorAction SilentlyContinue
$setEnv = ($envPairs | ForEach-Object { "set `"$_`" && " }) -join ''

# Takeover goes through dev-takeover.ps1 rather than being inlined here: it
# has to close the app, wait for the port, start the dev stack, and - the part
# that matters - put the installed app back if the dev build never comes up.
# That sequence outliving this process is the whole point, so it is spawned
# the same detached way.
if ($Takeover -and $installed.Count -gt 0) {
  $stage2 = Join-Path $PSScriptRoot 'dev-takeover.ps1'
  $exe = ($installed | Select-Object -First 1).ExecutablePath
  $pids = ($installed | ForEach-Object { $_.ProcessId }) -join ','
  # Not $args: that is an automatic variable, and shadowing it inside a script
  # is the kind of thing that works until the day it does not.
  $stageArgs = "-NoProfile -ExecutionPolicy Bypass -File `"$stage2`" -Repo `"$Repo`" " +
               "-InstalledExe `"$exe`" -KillPids $pids -Port $Port"
  $spawned = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
    CommandLine = "powershell.exe $stageArgs"
  }
  if ($spawned.ReturnValue -ne 0) {
    Say "FAILED to spawn the takeover (code $($spawned.ReturnValue)); nothing was closed"
    return
  }
  Say "takeover running detached (pid $($spawned.ProcessId))"
  Say 'it waits 10s, closes the app, then builds; the app comes back if the build fails'
  Say "log: $Log"
  return
}

$command = "cmd.exe /c cd /d `"$Repo`" && $setEnv npm run tauri dev >> `"$Log`" 2>&1"

# Win32_Process.Create, not Start-Process: the new process belongs to the WMI
# host, so it outlives this script and whatever started it.
$result = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $command }
if ($result.ReturnValue -ne 0) {
  Say "FAILED to spawn dev stack (Win32_Process.Create returned $($result.ReturnValue))"
  return
}

Say "dev stack spawned (pid $($result.ProcessId)); cargo build takes a minute on a cold tree"
Say "log: $Log"
Say 'ONE launch. If the window does not appear, read the log - do not relaunch in a loop.'
