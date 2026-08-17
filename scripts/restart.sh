#!/usr/bin/env bash
#
# Safely restart the running Threadknot desktop app onto the freshly built release
# binary — WITHOUT this script (or the agent that launched it) being killed when
# the old instance dies.
#
# WHY THIS EXISTS
# ---------------
# An agent session (Claude Code / Codex) may itself be running *through* Threadknot.
# If it kills Threadknot directly from its own shell, its own tool call can die
# mid-restart and the app never comes back up. This script does the whole
# kill-and-relaunch inside its OWN process session so it finishes even if the
# caller is interrupted.
#
# HOW AGENTS SHOULD INVOKE IT (fire-and-forget, fully detached):
#
#   setsid nohup bash <path-to-checkout>/scripts/restart.sh \
#       >/dev/null 2>&1 </dev/null & disown
#
# Then wait ~10s and confirm via the log (do NOT relaunch on your own):
#
#   grep -q '=== done' /tmp/threadknot-restart.log   # then: cat /tmp/threadknot-restart.log
#   ss -ltnp | grep ':42800 '                    # threadknot listening = back up
#
# KEY IDEAS THAT MAKE IT RELIABLE
#   * setsid → new session, reparented to init, so killing Threadknot by PID does
#     not cascade into this script.
#   * It snapshots the LIVE instance's environment from /proc/<pid>/environ
#     BEFORE killing it, then relaunches with `env -i` + those exact vars, so
#     the new process inherits the working DISPLAY / WAYLAND_DISPLAY /
#     XDG_RUNTIME_DIR / DBUS / PATH the desktop launcher gave it. (Reproducing
#     the launcher env by hand is the usual reason a hand-restarted Tauri app
#     shows a blank window or "could not connect".)
#   * ONE launch attempt, then it only *verifies* (polls port 42800). It never
#     relaunches in a loop — a crash-loop here has taken the whole machine down
#     before (coredump storm). If it reports FAIL, diagnose from the log; do
#     not hammer it.
#
# Assumes the target binary already exists. For the release build:
#   cd threadknot && npx tauri build --no-bundle
# NOT plain `cargo build --release` — that ships a binary which loads the dev
# server and shows "Could not connect to localhost". For the dev build, a plain
# `cargo build` in src-tauri/ is what the dev launcher runs against.
#
# Which binary gets relaunched is DERIVED from the live process, not fixed —
# see the note further down.
set -u
LOG=/tmp/threadknot-restart.log
# Derived from this script's own location, not hardcoded — every checkout of
# this repo lives somewhere different, and a baked-in path silently restarts
# somebody else's binary (or nothing at all).
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_BIN="$REPO/src-tauri/target/release/threadknot"
PORT=42800
exec >>"$LOG" 2>&1
echo "==================== restart $(date '+%F %T') ===================="

# Let the launching shell return first (it may be a child of threadknot).
sleep 3

# Identify the live instance by the port it owns (unambiguous), then fallbacks.
OLDPID="$(ss -ltnp 2>/dev/null | grep ":$PORT " | grep -oP 'pid=\K[0-9]+' | head -1)"
# Fallback matches either build, since the dev instance runs the debug binary,
# and matches a RELATIVE path because that is what the launchers actually exec
# (the live instance's cmdline is literally `target/debug/threadknot`). The `$`
# anchor keeps it off `threadknot-headless`. The port check above is the
# authoritative one; this only covers an instance that failed to bind.
[ -z "${OLDPID:-}" ] && OLDPID="$(pgrep -f 'target/(debug|release)/threadknot$' | head -1)"
echo "old pid: ${OLDPID:-none}"

# Relaunch the SAME build that is running, not always the release one.
#
# The desktop launchers are "Threadknot" (release binary) and "Threadknot (Dev)"
# (`~/.local/bin/threadknot-dev-app`, which runs the DEBUG binary against vite).
# `BIN` used to be hardcoded to release, so restarting a dev instance silently
# swapped it for a release build — which looks like "my HMR stopped working" and
# is genuinely baffling if you don't know this script did it. Worse, the two
# builds can be different commits, so "restart onto the new build" would restart
# onto the wrong one.
#
# /proc/<pid>/exe still resolves after the file is replaced by a rebuild, with
# " (deleted)" appended to the link text — so strip that, then check the path is
# there now (the rebuild put a fresh file at the same location).
BIN="$RELEASE_BIN"
if [ -n "${OLDPID:-}" ]; then
  LIVE_EXE="$(readlink "/proc/$OLDPID/exe" 2>/dev/null || true)"
  LIVE_EXE="${LIVE_EXE% (deleted)}"
  if [ -n "$LIVE_EXE" ] && [ -x "$LIVE_EXE" ]; then
    BIN="$LIVE_EXE"
  elif [ -n "$LIVE_EXE" ]; then
    echo "WARN: live binary $LIVE_EXE is gone; falling back to the release build"
  fi
fi
echo "relaunching: $BIN"

# Refuse to relaunch a release binary that `tauri build` did not produce.
#
# On Linux there is no copy-to-live step: the binary this script execs is the
# very file a rebuild overwrites, so a `cargo build --release` lands directly in
# the path about to be launched. That build embeds the same Vite assets but
# points its webview at devUrl (localhost:1430), so every window shows
# ERR_CONNECTION_REFUSED while the Rust server behind it runs fine — and the
# port check at the bottom of this script reports it as a healthy restart. That
# is exactly what happened on 2026-08-16 (on Windows, which has since grown this
# same guard).
#
# rebuild.sh stamps threadknot.build.json beside the release binary with its
# SHA256; a later cargo build overwrites the binary and leaves the stamp, so the
# hash stops matching. Checked BEFORE the running instance is killed, so a bad
# build costs nothing.
#
# ONLY for the release binary. `BIN` is derived from the live process above, and
# a dev instance legitimately runs target/debug/threadknot, which nothing stamps
# — demanding one there would refuse every dev restart.
if [ "$BIN" = "$RELEASE_BIN" ]; then
  STAMP="$(dirname "$BIN")/threadknot.build.json"
  if [ ! -f "$STAMP" ]; then
    echo "FAIL: no threadknot.build.json beside $BIN — build it with ./rebuild.sh."
    echo "      Not restarting; the running app is untouched."
    exit 1
  fi
  # Read the two fields with plain text tools: this script must not depend on
  # jq being installed to decide whether the app may restart.
  WANT="$(sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]*\)".*/\1/p' "$STAMP" | head -1)"
  KIND="$(sed -n 's/.*"kind"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$STAMP" | head -1)"
  GOT="$(sha256sum "$BIN" | cut -d' ' -f1)"
  if [ "$KIND" != "tauri-build" ] || [ -z "$WANT" ] || [ "$WANT" != "$GOT" ]; then
    echo "FAIL: $BIN does not match its build stamp — it was rebuilt by something"
    echo "      other than ./rebuild.sh (most likely cargo build --release)."
    echo "      Not restarting; the running app is untouched."
    exit 1
  fi
  echo "verified: tauri build stamp matches ($GOT)"
else
  echo "dev binary — skipping the tauri-build stamp check"
fi

# Snapshot the launcher env + cwd from the LIVE process before it dies.
# NOTE: write with a redirect, never `cp` — /proc/<pid>/environ is mode 0400, so
# `cp` stamps that onto ENVFILE and every LATER restart fails to overwrite it
# ("Permission denied") and silently relaunches with a stale snapshot. That is
# invisible until a reboot changes DISPLAY/DBUS, and then the app comes back
# with a blank window — exactly what this env capture exists to prevent.
ENVFILE=/tmp/threadknot-restart.env
OLDCWD="$HOME"
if [ -n "${OLDPID:-}" ] && [ -r "/proc/$OLDPID/environ" ]; then
  if ! cat "/proc/$OLDPID/environ" >"$ENVFILE"; then
    echo "FAIL: cannot write $ENVFILE — fix its ownership/permissions and retry"
    exit 1
  fi
  OLDCWD="$(readlink "/proc/$OLDPID/cwd" 2>/dev/null || echo "$HOME")"
  echo "captured env + cwd ($OLDCWD) from /proc/$OLDPID"
else
  cat /proc/self/environ >"$ENVFILE" 2>/dev/null || : >"$ENVFILE"
  echo "WARN: fell back to current env"
fi

# Graceful stop, then force if needed.
if [ -n "${OLDPID:-}" ]; then
  kill -TERM "$OLDPID" 2>/dev/null
  for _ in $(seq 1 24); do kill -0 "$OLDPID" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$OLDPID" 2>/dev/null; then
    echo "did not exit on TERM; sending KILL"
    kill -KILL "$OLDPID" 2>/dev/null
    sleep 1
  fi
  echo "old instance stopped"
fi

# Wait for the port to free up before starting the new one.
for _ in $(seq 1 20); do ss -ltn 2>/dev/null | grep -q ":$PORT " || break; sleep 0.5; done

# Rebuild the exact env (NUL-delimited) and launch detached in its own session.
mapfile -d '' ENVVARS < "$ENVFILE"
cd "$OLDCWD" 2>/dev/null || cd "$HOME" || cd /
echo "launching new binary ($(stat -c %y "$BIN" 2>/dev/null || echo '??'))"
setsid env -i "${ENVVARS[@]}" "$BIN" >>"$LOG" 2>&1 &
echo "launched, setsid pid $!"

# Verify it comes back up (port listening). ONE attempt, no relaunch loop.
UP=""
for _ in $(seq 1 60); do
  if ss -ltn 2>/dev/null | grep -q ":$PORT "; then UP=1; break; fi
  sleep 0.5
done
if [ -n "$UP" ]; then
  echo "OK: threadknot is back up — $PORT listening"
  ss -ltnp 2>/dev/null | grep ":$PORT "
else
  echo "FAIL: $PORT not listening after 30s — check above; NOT relaunching"
fi
echo "==================== done $(date '+%F %T') ===================="
