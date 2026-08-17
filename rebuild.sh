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

# Every cargo invocation goes through scripts/cargo-env.sh — see that file. In
# short: a build started from a shell inside the running app inherits cargo's
# own build variables from the `cargo run` that launched it, ring's build script
# fingerprints several of them, and a build that records different values than
# the last one rebuilds the rustls/reqwest half of the tree plus this crate for
# nothing. The wrapper normalises them so the release cache stays warm no matter
# where the build was started from.
if [ "${1:-}" = "--bundle" ]; then
  shift
  echo "==> Building Threadknot with OS bundles (embeds UI via tauri build)…"
  scripts/cargo-env.sh npm run tauri build -- "$@"
else
  echo "==> Building Threadknot (embeds UI via tauri build, no bundle)…"
  scripts/cargo-env.sh npm run tauri build -- --no-bundle "$@"
fi

# Cargo appends .exe on Windows. Git Bash would resolve the extensionless name
# anyway (MSYS maps foo -> foo.exe), but only the real name is worth printing,
# and a non-MSYS shell would not find it at all. Prefer .exe where it exists.
#
# Written as `if`, not `[ -f x ] && y`: under `set -e` a failing AND-list at the
# top level exits the script, so the bare form would abort every unix build.
BIN="src-tauri/target/release/threadknot"
if [ -f "$BIN.exe" ]; then BIN="$BIN.exe"; fi
HEADLESS="src-tauri/target/release/threadknot-headless"
if [ -f "$HEADLESS.exe" ]; then HEADLESS="$HEADLESS.exe"; fi

# Sanity-check that the freshly built web UI actually got embedded in the binary.
# Search the binary directly with `grep -a` rather than piping `strings` into it:
# strings ships with binutils and is absent from Git Bash on Windows, and the
# pipeline form is a false negative anyway under `set -o pipefail` (grep closes
# the pipe on the first match, strings dies with SIGPIPE).
#
# Match with `-e`, not a bare argument: vite hashes routinely begin with a dash
# (index--ZNxb27G), which grep would otherwise read as options.
#
# `grep -m1` rather than `grep | head -1`: head closing the pipe early kills grep
# with SIGPIPE, which `set -o pipefail` turns into a failed assignment. The
# trailing `|| true` keeps a missing dist/index.html on the WARNING path below
# instead of exiting here with nothing explained.
HASH="$(grep -m1 -o 'assets/index-[^"]*\.js' dist/index.html 2>/dev/null | sed 's#.*index-##; s#\.js##' || true)"
EMBEDDED="$(grep -a -c -F -e "$HASH" "$BIN" 2>/dev/null || true)"
[ -n "$EMBEDDED" ] || EMBEDDED=0

if [ ! -f "$BIN" ]; then
  echo "==> ERROR: the build produced no binary at $BIN." >&2
  exit 1
elif [ -z "$HASH" ]; then
  # The check itself could not run. Say so rather than condemning a build on the
  # strength of a test that never happened - that is how a good build gets
  # thrown away.
  echo "==> WARNING: could not read the asset hash from dist/index.html;" >&2
  echo "    skipped the embedded-UI check. Confirm the window is not blank." >&2
elif [ "$EMBEDDED" -gt 0 ]; then
  echo "==> OK: web UI ($HASH) is embedded in $BIN"
else
  echo "==> ERROR: web UI is NOT embedded — this is the broken dev build." >&2
  echo "    The desktop window would show 'Could not connect to localhost'." >&2
  exit 1
fi

# Stamp the binary so the promoters can tell what produced it.
#
# The embedded-UI grep above is a useful sanity check but it is NOT a way to
# tell a `tauri build` binary from a `cargo build --release` one, and the
# promoters (launch-threadknot.ps1, scripts/restart-windows.ps1) used to treat
# it as exactly that. It cannot be: cargo embeds the same Vite assets, it just
# builds a webview that navigates to devUrl (localhost:1430) instead of loading
# them. On 2026-08-16 a cargo build sailed through that check, replaced a
# working install, and every window it opened showed ERR_CONNECTION_REFUSED
# while the Rust server behind it was perfectly healthy.
#
# This script is the only one that shells out to `tauri build`, so the stamp it
# writes is the honest signal. It binds to the exact bytes just produced: a
# later `cargo build --release` overwrites the binary and leaves the stamp
# untouched, the hash stops matching, and the swap is refused rather than
# silently replacing a working app with a broken one.
STAMP="$(dirname "$BIN")/threadknot.build.json"
SHA="$(sha256sum "$BIN" | cut -d' ' -f1)"
SIZE="$(wc -c < "$BIN" | tr -d '[:space:]')"
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
cat > "$STAMP" <<EOF
{
  "kind": "tauri-build",
  "sha256": "$SHA",
  "size": $SIZE,
  "assetHash": "$HASH",
  "commit": "$COMMIT",
  "builtAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "builtBy": "rebuild.sh"
}
EOF
echo "==> Stamped $STAMP ($SHA)"

echo "==> Done. Desktop app:  $BIN"
echo "==> Headless LAN server: $HEADLESS"
