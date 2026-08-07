#!/usr/bin/env bash
# Build Threadknot the ONLY way that embeds the web UI into the desktop binary.
#
# Do NOT use `cargo build --release` for the desktop app: that produces a
# dev-mode binary whose webview points at the Vite dev server (localhost:1430),
# so the window shows a blank "Could not connect to localhost" page. Only the
# Tauri CLI (`tauri build`) compiles the web UI into the executable.
#
# Usage: ./rebuild.sh              # build binary (no OS bundle)
#        ./rebuild.sh --bundle     # also produce deb/rpm/appimage
set -euo pipefail
cd "$(dirname "$0")"

if [ "${1:-}" = "--bundle" ]; then
  shift
  echo "==> Building Threadknot with OS bundles (embeds UI via tauri build)…"
  npm run tauri build -- "$@"
else
  echo "==> Building Threadknot (embeds UI via tauri build, no bundle)…"
  npm run tauri build -- --no-bundle "$@"
fi

BIN="src-tauri/target/release/threadknot"

# Sanity-check that the freshly built web UI actually got embedded in the binary.
# Use `grep -c` into a variable (not `grep -q` in a pipeline): under
# `set -o pipefail`, `strings | grep -q` reports failure because grep closes the
# pipe on first match and strings dies with SIGPIPE — a false negative.
HASH="$(grep -o 'assets/index-[^"]*\.js' dist/index.html | head -1 | sed 's#.*index-##; s#\.js##')"
EMBEDDED="$(strings -n 6 "$BIN" | grep -Fc "$HASH" || true)"
if [ -n "$HASH" ] && [ "$EMBEDDED" -gt 0 ]; then
  echo "==> OK: web UI ($HASH) is embedded in $BIN"
else
  echo "==> ERROR: web UI is NOT embedded — this is the broken dev build." >&2
  echo "    The desktop window would show 'Could not connect to localhost'." >&2
  exit 1
fi

echo "==> Done. Desktop app:  $BIN"
echo "==> Headless LAN server: src-tauri/target/release/threadknot-headless"
