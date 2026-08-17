#!/usr/bin/env bash
# Build Threadknot as installable Linux packages: .deb, .rpm, and a portable
# AppImage. Linux counterpart of build-mac.sh — same rule as rebuild.sh: only
# `tauri build` embeds the web UI, never ship a plain `cargo build --release`.
#
# Usage: ./build-linux.sh                 # deb + rpm + AppImage for this arch
#        ./build-linux.sh --deb-only      # skip the AppImage (no network fetch)
#
# The deb/rpm are built by Tauri's pure-Rust packagers, so they build fine on
# any distro (an Arch box can produce the .deb). The AppImage step downloads
# linuxdeploy on first run. The AppImage is the download-and-run answer for
# every distro, Arch included — there is no native pacman bundle target.
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(uname)" != "Linux" ]; then
  echo "==> ERROR: build-linux.sh only runs on Linux (Tauri cannot cross-compile" >&2
  echo "    Linux packages — it links webkit2gtk/GTK from the host). Run this on" >&2
  echo "    the Linux box, or dispatch CI: gh workflow run build.yml" >&2
  exit 1
fi

# Tauri's Linux system deps. Check via pkg-config and report the exact install
# command for this distro rather than sudo-ing on the user's behalf.
missing=()
for lib in webkit2gtk-4.1 gtk+-3.0 librsvg-2.0 openssl; do
  pkg-config --exists "$lib" || missing+=("$lib")
done
command -v patchelf >/dev/null || missing+=(patchelf)
if [ "${#missing[@]}" -gt 0 ]; then
  echo "==> ERROR: missing system deps: ${missing[*]}" >&2
  echo "    Debian/Ubuntu: sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libgtk-3-dev libssl-dev patchelf" >&2
  echo "    Fedora:        sudo dnf install -y webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel gtk3-devel openssl-devel patchelf" >&2
  echo "    Arch:          sudo pacman -S --needed webkit2gtk-4.1 libayatana-appindicator librsvg gtk3 openssl patchelf" >&2
  exit 1
fi

BUNDLES="deb,rpm,appimage"
if [ "${1:-}" = "--deb-only" ]; then
  shift
  BUNDLES="deb"
fi

# linuxdeploy is itself an AppImage, and it runs more AppImages (its gtk and
# appimage plugins) as child processes. Mounting those needs FUSE, which is
# absent or unusable often enough — containers, hosts without the module, a
# kernel that only has fuse3 — that the failure is the norm rather than the
# exception, and Tauri reports it only as `failed to run linuxdeploy`, which
# names the symptom and not the cause. Extract-and-run skips mounting entirely:
# each AppImage is unpacked to a temp dir and executed from there. Costs a
# little disk and startup time on a step that already takes minutes.
export APPIMAGE_EXTRACT_AND_RUN=1

# cargo-env.sh strips the build variables cargo injects into shells spawned from
# inside the running app, keeping the release cache warm — see that file.
echo "==> Building Threadknot packages ($BUNDLES) — embeds UI via tauri build…"
scripts/cargo-env.sh npm run tauri build -- --bundles "$BUNDLES" ${@+"$@"}

BIN="src-tauri/target/release/threadknot"
BUNDLE_DIR="src-tauri/target/release/bundle"

# Same embed sanity-check as rebuild.sh: the fresh web bundle's hash must appear
# in the shipped binary, or this is the broken dev build that dials localhost.
#
# Match with `-e`: vite hashes routinely begin with a dash (index--ZNxb27G), and
# grep reads a leading dash as options, so the bare form fails on roughly one
# build in thirty — on a binary that is perfectly fine. `grep -a` over the binary
# rather than `strings | grep` drops the binutils dependency and the SIGPIPE
# false negative pipefail turns into a failure; `-m1` replaces `| head -1` for
# that same reason.
#
# Unlike rebuild.sh, a check that cannot run is fatal here: these are the
# packages that go to users, and an unverified one must not ship.
# A missing binary is checked FIRST and named as such. Without this the grep
# below simply finds nothing, and the build reports "web UI is NOT embedded —
# this is the broken dev build", sending the reader to diagnose an embedding
# problem on a binary that was never produced.
if [ ! -f "$BIN" ]; then
  echo "==> ERROR: the build produced no binary at $BIN." >&2
  exit 1
fi
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
echo "==> OK: web UI ($HASH) is embedded in $BIN"

echo "==> Done. Packages:"
ls -1 "$BUNDLE_DIR"/deb/*.deb "$BUNDLE_DIR"/rpm/*.rpm "$BUNDLE_DIR"/appimage/*.AppImage 2>/dev/null | sed 's/^/    /'
echo "    Install:  deb: sudo apt install ./<file>.deb   rpm: sudo dnf install ./<file>.rpm"
echo "              AppImage (any distro, incl. Arch): chmod +x <file>.AppImage && ./<file>.AppImage"
