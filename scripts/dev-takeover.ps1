# Stage 2 of the takeover: hand :42800 and the real data directory from the
# installed app to a fast-refresh dev build. Spawned detached by
# dev-windows.ps1 -Takeover; never run it directly unless you know the dev
# stack is already stopped.
#
# WHY A SEPARATE PROCESS: the agent session usually runs THROUGH the app being
# closed. Killing that app from the caller's own shell cuts the caller off
# mid-command, leaving nothing alive to start the replacement. This script is
# created through Win32_Process.Create, so it belongs to the WMI host and
# finishes no matter what happens to whoever asked for it.
#
# WHY A WATCHDOG: the failure mode here is the worst one in this repo - the
# user's app is gone, the agent that could fix it is gone with it, and a dev
# build that will not start leaves them with nothing. So if the dev server has
# not claimed the port within the deadline, the installed app is put back and
# the log says why.
#
# NOTE: keep this file ASCII-only. PowerShell 5.1 reads BOM-less files as ANSI
# and non-ASCII punctuation breaks parsing.

param(
  [Parameter(Mandatory = $true)][string]$Repo,
  [Parameter(Mandatory = $true)][string]$InstalledExe,
  [int[]]$KillPids = @(),
  [int]$Port = 42800,
  [int]$GraceSeconds = 10,
  [int]$DeadlineSeconds = 240
)

$ErrorActionPreference = 'Continue'
$Log = Join-Path $env:TEMP 'threadknot-dev.log'

function Say($m) {
  "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')  [takeover] $m" |
    Add-Content $Log -ErrorAction SilentlyContinue
}

function Port-Held([int]$p) {
  try { @(Get-NetTCPConnection -LocalPort $p -State Listen -ErrorAction Stop).Count -gt 0 }
  catch { $false }
}

Say "starting; installed exe $InstalledExe"

# Grace: the caller is mid-sentence to the user when this fires. Let that land
# before the window it is displayed in disappears.
if ($GraceSeconds -gt 0) { Start-Sleep -Seconds $GraceSeconds }

foreach ($id in $KillPids) {
  try { Stop-Process -Id $id -Force -ErrorAction Stop; Say "closed pid $id" }
  catch { Say "pid $id already gone" }
}

# A killed process keeps its listener for a moment. Starting into that window
# is how the dev server ends up bound to nothing while looking healthy.
$waited = 0
while ((Port-Held $Port) -and $waited -lt 20) { Start-Sleep -Seconds 1; $waited++ }
if (Port-Held $Port) {
  Say "port $Port still held after ${waited}s - aborting, installed app left alone"
  return
}
Say "port $Port free after ${waited}s"

$command = "cmd.exe /c cd /d `"$Repo`" && set `"THREADKNOT_PORT=$Port`" && npm run tauri dev >> `"$Log`" 2>&1"
$spawn = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $command }
if ($spawn.ReturnValue -ne 0) {
  Say "FAILED to spawn dev stack (code $($spawn.ReturnValue)) - restoring installed app"
  Start-Process $InstalledExe
  return
}
Say "dev stack spawned (pid $($spawn.ProcessId)); waiting up to ${DeadlineSeconds}s for :$Port"

$waited = 0
while (-not (Port-Held $Port) -and $waited -lt $DeadlineSeconds) { Start-Sleep -Seconds 2; $waited += 2 }

if (Port-Held $Port) {
  Say "dev build is live on :$Port with fast refresh - edits under src/ apply in about a second"
} else {
  Say "dev build did not come up within ${DeadlineSeconds}s - restoring the installed app"
  Say 'read the cargo output above this line for the reason'
  Start-Process $InstalledExe
}
