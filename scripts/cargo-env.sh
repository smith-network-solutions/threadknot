#!/usr/bin/env bash
#
# Run a cargo / tauri / npm command with the environment *cargo itself injected*
# stripped out, so every entry point shares one warm build cache.
#
# WHY THIS EXISTS
# ---------------
# The dev app is started by `tauri dev`, which is `cargo run`. Cargo exports its
# own build variables into the program it launches — CARGO_MANIFEST_DIR,
# CARGO_PKG_*, OUT_DIR, DEBUG, LD_LIBRARY_PATH, RUST_RECURSION_COUNT — so every
# process Threadknot spawns inherits them, including the agent sessions running
# inside it. A shell opened in the app therefore looks, to cargo, like the
# inside of a build script.
#
# That is not cosmetic. `ring`'s build script reads CARGO_MANIFEST_DIR,
# CARGO_PKG_NAME, CARGO_PKG_VERSION_*, DEBUG and OUT_DIR through a helper that
# emits `cargo:rerun-if-env-changed` for each one, and cargo records those
# values from *its own* process environment. Build from inside the app and it
# records Some("/media/.../src-tauri"); build from a terminal or a desktop
# launcher and it records None. Every switch between the two reruns ring's
# build script, and the invalidation cascades through rustls, rustls-webpki,
# tokio-rustls, hyper-rustls, reqwest, rcgen, tungstenite and finally this
# crate: ~40s of rebuilding with not one source file changed.
#
# Measured here: two builds in a row from the same environment finish in 0.2s.
# Alternating environments costs 40-60s every single time, in both directions.
# That is the whole "why does it always rebuild" mystery — it is not Tauri, not
# the git-derived version in build.rs, and not sccache.
#
# Normalising these to *unset* — what a plain terminal has — makes the agent
# shell, the desktop launcher and a hand-typed `cargo build` agree.
#
# Usage:
#   scripts/cargo-env.sh cargo build
#   scripts/cargo-env.sh npm run tauri dev -- --no-watch
#
# CARGO_HOME and RUSTUP_HOME are deliberately kept: nothing fingerprints them,
# and clearing a non-default one would point cargo at the wrong registry.
set -u

# Fixed names cargo sets for `cargo run` / build scripts.
unset_args=(
  -u OUT_DIR
  -u LD_LIBRARY_PATH
  -u RUSTUP_TOOLCHAIN
  -u RUST_RECURSION_COUNT
  -u DEBUG
  -u PROFILE
  -u OPT_LEVEL
  -u NUM_JOBS
  -u HOST
  -u TARGET
)

# Everything else cargo prefixes with CARGO_ (CARGO_PKG_*, CARGO_MANIFEST_*,
# CARGO_CFG_*, CARGO_BIN_NAME, the CARGO pointer itself, ...) — matched by
# prefix rather than listed, because the set grows between cargo releases.
while IFS= read -r var; do
  case "$var" in
    CARGO_HOME) ;;                       # keep: not fingerprinted, and load-bearing
    *) unset_args+=(-u "$var") ;;
  esac
done < <(env | sed -n 's/^\(CARGO[A-Z_0-9]*\)=.*/\1/p')

exec env "${unset_args[@]}" "$@"
