# Threadknot — every coding agent on one thread

A Tauri (Rust) desktop app that drives **Claude Code**, **OpenAI Codex**, **Kimi
Code**, and remote **hermes-agent** gateways natively over their wire protocols
— no terminal wrapping, no Node server — and serves the same UI to any browser
on the LAN, so the app is usable from a phone.

> Shipped as **Armada** until the 2026-08 rename. See "Rename compatibility"
> below before touching anything that reads from disk.

> Owner: Spencer Smith (Smith Network Solutions).

## Layout

- **Rust backend** (`src-tauri/src/`): `server.rs` (axum :42800, token-gated
  `/ws`, serves `dist/` on LAN), `agents/claude.rs` (claude CLI stream-json +
  control_request permission protocol), `agents/codex.rs` (codex app-server
  JSON-RPC), `agents/kimi.rs`, `agents/hermes.rs` (remote gateways: Runs API
  over HTTP/SSE), `agents/mod.rs` (hub: event persistence + fanout),
  `library.rs`/`catalog.rs` (the Library: skills written into each CLI's own
  skills dir + an MCP-server registry injected into every driver at spawn),
  `store.rs` (the data dir), `protocol.rs` (normalized event schema).
  Contract: `docs/PROTOCOL.md`.
- **Frontend**: React/TS in `src/`, plain CSS, no state libs. Works in the Tauri
  webview and any phone browser (LAN URL + token in Settings popover). For the
  whole-screen flash — what re-rendered, what remounted, and which action caused
  it, readable from a headless browser — arm `?tktrace=1` and read
  **`docs/RENDER-FORENSICS.md`**.
- **Mobile app** (`mobile/`): Expo SDK 57 companion — biometric-locked WebView
  shell with multi-server switching + push notifications. Read
  **`docs/MOBILE.md`** first.
- **Dispatch** (`dispatch.rs`, `exec.rs`, `mcp_fleet.rs`): one thread hands a
  brief to another agent — a different harness, a different machine — and gets
  a report back. Read **`docs/USING-DISPATCH.md`** before changing any of it.
- **Auth**: uses the installed `claude` / `codex` / `kimi` CLIs' own
  subscription logins.

## Verify gate

`cargo build && cargo clippy` (in `src-tauri/`), `cargo test`, `npm run build`.
Mobile: `npm run typecheck` + `npx expo-doctor` in `mobile/`.

Smoke test: run `./src-tauri/target/debug/threadknot-headless` and drive `/ws`
with a script. Real agent turns work headless.

## Building and running

- **Cargo's own build environment must never reach a build.** `tauri dev` starts
  the app with `cargo run`, so the app — and everything it spawns, agent CLIs and
  PTY shells included — inherits `CARGO_MANIFEST_DIR`, `CARGO_PKG_*`, `OUT_DIR`,
  `DEBUG`. `ring`'s build script fingerprints exactly those, and cargo records
  them from its own environment, so a build started inside the app and one
  started from a terminal each invalidate the other: ring, rustls, tokio-rustls,
  hyper-rustls, reqwest, rcgen, tungstenite and this crate all recompile, ~40s,
  with nothing changed. Two defences, keep both:
  - `scrub_cargo_env()` in `lib.rs`, called first thing in both `main`s, drops
    them before anything spawns. This is what protects *other* Rust projects an
    agent builds from inside Threadknot.
  - `scripts/cargo-env.sh` wraps a command with the same set cleared, for builds
    started from a shell that predates the fix or from another checkout:
    `scripts/cargo-env.sh cargo build`. `rebuild.sh` and both dev launchers
    already go through it.

  Either way the check is the same: two builds in a row with no edit should
  finish in ~0.2s. If the second one recompiles, an environment is drifting.
- **Rebuild the desktop app**: `scripts/cargo-env.sh npx tauri build --no-bundle`
  (NOT plain `cargo build --release` — that ships a binary that loads the dev
  server and shows "Could not connect to localhost").
- **Restart the running app onto a new build**: use `scripts/restart.sh`,
  launched **detached** so it survives the old instance dying — if you are
  driving an agent *through* Threadknot, a direct kill cuts your own tool call
  mid-restart:
  `setsid nohup bash scripts/restart.sh >/dev/null 2>&1 </dev/null & disown`.
  Confirm via `/tmp/threadknot-restart.log` + `ss -ltnp | grep :42800`.
  It launches **once** and only verifies — never wrap it in a relaunch loop.
- **Full dev guide + hard-won gotchas** (Wayland webkit flag, desktop-launch
  PATH, Tauri capabilities, per-driver wire facts): **`docs/DEVELOPMENT.md`** —
  read it before changing anything.

## Rename compatibility (do not "clean this up")

The rebrand deliberately left four Armada-era names load-bearing on disk.
Each has a test; removing one silently breaks existing installs:

- `store.rs` `data_dir_in()` — prefers `~/.threadknot`, falls back to
  `~/.armada`. **Nothing is migrated.** An existing install keeps using the
  directory it already filled (browser profiles with live logins, paired
  phones, the LAN token). `ARMADA_DATA_DIR` / `ARMADA_PORT` still work.
- `library.rs` `is_marker()` — also reads `.armada-library.json`, or skills
  installed by the old build become foreign folders the Library can't delete.
- `discovery.ts` `readStoredToken()` — carries a stored `armada.token` over,
  so a phone holding the LAN URL isn't logged out.
- `artifacts.rs` `EXCLUDED_DIRS` — keeps `.armada` excluded forever, or old
  user attachments resurface as freshly produced deliverables.

Two words are **not** rebrand leftovers: **"ship"** is the software verb here
("ships a skill"), and **"fleet"** is a real domain concept (fleet view vs solo
window). Renaming "fleet" is a product decision, not a chore.

## Operational rules (learned the hard way)

- **Launching GUI apps is allowed**, but launch at most **once** and **never in
  a retry loop** — a crash-loop here once caused a coredump storm that took the
  machine down. If a launch fails, diagnose via **logs**, don't hammer relaunch.
- The Library writes skills into the CLIs' own **global** dirs
  (`~/.claude/skills/`, `~/.codex/skills/`, `~/.kimi-code/skills/`), which
  `THREADKNOT_DATA_DIR` does **not** isolate, and `publish()` removes the
  destination first. Sandbox `HOME` before exercising installs in a second
  instance, or it will clobber the skills the live app is using.
- Run a second instance side-by-side with `THREADKNOT_DATA_DIR=/tmp/... 
  THREADKNOT_PORT=42801`.

## The hosted relay is not in this repository

Everything here works with no account and no internet. The **hosted relay** —
the optional paid tier that gives an installation a stable public HTTPS origin —
is a separate, private service, and it is deliberately not part of this
repository.

What *is* here is the client half: `src-tauri/src/connector.rs`, which dials
out to it, and **`relay-protocol/`**, the wire contract the two speak.
`relay-protocol` is canonical here because the connector ships it, and there is
deliberately one copy: two definitions of "what gets signed" is the pair that
drifts silently, and the failure that drift produces — a signature that verifies
on one side and not the other — is indistinguishable from a network fault. So
changing a signed message layout, a frame, or a DTO in that crate is a change to
**both** ends at once.

`/relay/` and `/console/` are in `.gitignore` on purpose. Do not remove those
lines to "tidy up".

<!-- Maintainer-only context (working tree layout, release machinery). Not in
     the repository; absent for contributors, which is fine. -->
@CLAUDE.local.md
