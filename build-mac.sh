#!/usr/bin/env bash
# Build Threadknot as a real macOS app: Threadknot.app plus a drag-to-Applications
# .dmg installer. This is the macOS counterpart of rebuild.sh — same rule applies:
# only `tauri build` embeds the web UI into the binary, so never ship a plain
# `cargo build --release` binary.
#
# Usage: ./build-mac.sh                 # native-arch .app + .dmg
#        ./build-mac.sh --universal     # single .app/.dmg for Apple Silicon + Intel
#
# Signing: unsigned builds get an ad-hoc signature, which runs fine on the
# machine that built it, but other Macs will quarantine a downloaded copy
# (right-click → Open, or `xattr -dr com.apple.quarantine`). To sign for real
# distribution, export APPLE_SIGNING_IDENTITY="Developer ID Application: …"
# (plus APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID to notarize) before running —
# Tauri picks them up automatically.
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(uname)" != "Darwin" ]; then
  echo "==> ERROR: build-mac.sh only runs on macOS. Use rebuild.sh elsewhere." >&2
  exit 1
fi

TARGET_ARGS=()
BUNDLE_DIR="src-tauri/target/release/bundle"
if [ "${1:-}" = "--universal" ]; then
  shift
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$t" >/dev/null
  done
  TARGET_ARGS=(--target universal-apple-darwin)
  BUNDLE_DIR="src-tauri/target/universal-apple-darwin/release/bundle"
fi

# cargo-env.sh strips the build variables cargo injects into shells spawned from
# inside the running app, keeping the release cache warm — see that file.
echo "==> Building Threadknot.app + .dmg (embeds UI via tauri build)…"
# ${arr[@]+…} guards: macOS ships Bash 3.2, where expanding an empty array trips set -u.
scripts/cargo-env.sh npm run tauri build -- --bundles app,dmg ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"} ${@+"$@"}

APP="$BUNDLE_DIR/macos/Threadknot.app"
BIN="$APP/Contents/MacOS/threadknot"
DMG="$(ls -t "$BUNDLE_DIR"/dmg/Threadknot_*.dmg 2>/dev/null | head -1 || true)"

# Same embed sanity-check as rebuild.sh: the fresh web bundle's hash must appear
# in the shipped binary, or this is the broken dev build that dials localhost.
#
# Match with `-e`: vite hashes routinely begin with a dash (index--ZNxb27G), and
# grep reads a leading dash as options, so the bare form fails on roughly one
# build in thirty — on a binary that is perfectly fine. `grep -a` over the binary
# rather than `strings | grep` also means this no longer needs the Xcode command
# line tools installed just to verify a build; `-m1` replaces `| head -1`, whose
# SIGPIPE pipefail reports as a failure.
#
# Unlike rebuild.sh, a check that cannot run is fatal here: these are the
# packages that go to users, and an unverified one must not ship.
HASH="$(grep -m1 -o 'assets/index-[^"]*\.js' dist/index.html 2>/dev/null | sed 's#.*index-##; s#\.js##' || true)"
if [ -z "$HASH" ]; then
  echo "==> ERROR: no web bundle hash in dist/index.html — did the frontend build run?" >&2
  exit 1
fi
EMBEDDED="$(grep -a -c -F -e "$HASH" "$BIN" 2>/dev/null || true)"
[ -n "$EMBEDDED" ] || EMBEDDED=0
if [ "$EMBEDDED" -eq 0 ]; then
  echo "==> ERROR: web UI is NOT embedded — this is the broken dev build." >&2
  exit 1
fi
echo "==> OK: web UI ($HASH) is embedded in the app"

echo "==> Done."
echo "    App:       $APP"
if [ -n "$DMG" ]; then
  echo "    Installer: $DMG"
  echo "    Install:   open the dmg, drag Threadknot to Applications."
fi
if ! codesign -dv "$APP" 2>&1 | grep -q "Authority=Developer ID"; then
  echo "    Note: ad-hoc signed. On OTHER Macs a downloaded copy needs"
  echo "    right-click → Open (once), or: xattr -dr com.apple.quarantine /Applications/Threadknot.app"
fi
