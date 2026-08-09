# Threadknot — development & rebuild guide

Everything a new agent needs to change Threadknot and ship a working build. Read this
plus [`PROTOCOL.md`](PROTOCOL.md) before touching code.

## Layout

```
threadknot/
├── index.html, vite.config.ts, tsconfig.json, package.json   ← frontend root
├── src/                        ← React/TS UI (see below)
├── src-tauri/
│   ├── Cargo.toml              ← two binaries: `threadknot` (Tauri) + `threadknot-headless`
│   ├── tauri.conf.json         ← devUrl :1430, frontendDist ../dist
│   ├── capabilities/default.json ← REQUIRED: grants plugin perms (dialog, core)
│   └── src/
│       ├── main.rs             ← Tauri entry (sets Wayland env), see gotcha #2
│       ├── lib.rs              ← builds ServerState, `server_info` command, Tauri setup
│       ├── bin/threadknot-headless.rs ← server without a window (smoke tests / pure LAN)
│       ├── protocol.rs         ← wire types (must match PROTOCOL.md + src/lib/protocol.ts)
│       ├── store.rs            ← ~/.threadknot persistence
│       ├── server.rs           ← axum: /ws, static serving, request dispatch, the three listeners
│       ├── ingress.rs          ← which listener a request came in by, and what that door allows
│       ├── mesh.rs             ← mesh certificate identity, per-peer credentials, the pairing proof
│       ├── peers.rs            ← the peer registry (peers.json): per-pair credentials + pinned CAs
│       ├── peernet.rs          ← live peer sockets, mDNS discovery, routing, splices, byte proxy
│       ├── connector.rs        ← the outbound relay connector (dials out; nothing listens)
│       ├── git.rs              ← multi-repo git: discovery + status/diff/stage/commit/branch/push (git.* requests)
│       ├── claudex.rs         ← Claudex profiles + bridge sidecar supervisor
│       ├── library.rs          ← the Library: skill folders + MCP-server registry
│       ├── catalog.rs          ← shipped catalog (skill pointers, MCP templates)
│       ├── bundled.rs          ← Threadknot's own skills, include_str!'d from ../../skills/
│       └── agents/
│           ├── mod.rs          ← Hub (event persist+fanout), PATH resolution
│           ├── claude.rs       ← claude CLI stream-json driver (claude AND claudex)
│           ├── codex.rs        ← codex app-server JSON-RPC driver
│           └── kimi.rs         ← Kimi Code ACP JSON-RPC driver
├── skills/                     ← Threadknot's OWN clean-room document skills
│   └── {docx,xlsx,pptx,pdf}/   ← SKILL.md + scripts/, embedded via bundled.rs
├── relay-protocol/             ← connector↔relay wire contract; STANDALONE crate (see below)
├── scripts/mesh-smoke.py       ← two sandboxed instances, pairs them, asserts the mesh forms
└── docs/{PROTOCOL.md, DEVELOPMENT.md, protocol/*}
```

Frontend `src/`: `App.tsx` (wiring/actions), `lib/{ws,discovery,protocol,format}.ts`,
`state/{store.tsx,feed.ts}`, `components/{Sidebar,ThreadView,Composer,FeedItems,
SettingsPopover,DirPicker,Markdown,GitPane,icons}.tsx`, `styles.css`. No state libs, plain CSS.

## Versioning & update notes (CHANGELOG.md)

The app version is git-derived at build time (`src-tauri/build.rs`): it reads
`0.1.<commit count>`, so every commit bumps what the sidebar footer shows. No
one edits version fields by hand.

The footer version is interactive: hover shows when the update went live,
click opens client-facing update notes. Those notes come from `CHANGELOG.md`
at the repo root, embedded at compile time. **When you ship a user-visible
change, add a bullet to CHANGELOG.md** under a `## v<version> · <YYYY-MM-DD>`
header (version = commit count the release will land on). Write bullets in
plain user language: what changed and where to find it. Internal work (CI,
refactors, docs) does not get a bullet. The raw `git log` is also embedded
(`app.changelog` → `entries`) but is not shown to users.

## How to rebuild — READ THIS

### Desktop app (the release binary the launcher runs)

```bash
cd threadknot
npx tauri build --no-bundle        # frontend (npm run build) + release binary
```

> **GOTCHA #1 — never ship with plain `cargo build --release`.** The Tauri CLI sets
> env that makes the webview load the *embedded* `dist/` assets. A bare `cargo build`
> produces a binary that tries to load `http://localhost:1430` (the dev server) and
> shows **"Could not connect to localhost: Connection refused."** Always go through
> `npx tauri build` (or `npm run build` first, then cargo — but the CLI is the safe
> path). Confirm with: `strings src-tauri/target/release/threadknot | grep -c assets/index`
> → should be ≥1.

The launcher (`~/.local/share/applications/threadknot.desktop`) points at
`src-tauri/target/release/threadknot`, so after a successful `tauri build` just relaunch.

### Restarting the running app onto a new build (agents: use the script)

Don't kill + relaunch Threadknot by hand from your own shell. An agent session
(Claude Code / Codex / Kimi Code) may itself be running **through** Threadknot, so a direct kill
can cut your own tool call mid-restart and the app never comes back up. Use the
committed helper, launched **detached** so it finishes even if the caller dies:

```bash
setsid nohup bash threadknot/scripts/restart.sh >/dev/null 2>&1 </dev/null & disown
# then wait ~10s and confirm — do NOT relaunch yourself:
grep -q '=== done' /tmp/threadknot-restart.log && cat /tmp/threadknot-restart.log
ss -ltnp | grep ':42800 '                 # threadknot listening → back up
```

Why it's reliable (and why a naive `kill && ./threadknot &` isn't):

- **`setsid`** puts it in its own session, reparented to init, so killing Threadknot
  by PID never cascades into the restart script.
- It **snapshots the live instance's environment from `/proc/<pid>/environ`
  before killing it**, then relaunches with `env -i` + those exact vars. That
  reproduces the desktop launcher's `DISPLAY` / `WAYLAND_DISPLAY` /
  `XDG_RUNTIME_DIR` / `DBUS_SESSION_BUS_ADDRESS` / `PATH` — the usual reason a
  hand-restarted Tauri app comes up with a blank window or can't reach the
  compositor.
- It gives the launching shell a 3s head start (so your tool call returns before
  the kill), stops the old instance gracefully (TERM → KILL), waits for :42800 to
  free, launches **once**, then only **verifies** the port is listening again.

> **GOTCHA — never wrap this in a relaunch loop.** It launches exactly once and
> self-verifies. A crash-loop of GUI launches once caused a coredump storm that
> took the whole machine down (see the root `CLAUDE.md` operational rules). If it
> logs `FAIL`, diagnose from `/tmp/threadknot-restart.log`; don't hammer relaunch.

The script hardcodes only the two stable values — the release binary path and
port 42800. Verified working 2026-07-22 (this session's Hermes-fix rebuild).

### Dev loop (hot reload)

```bash
npm run tauri dev                  # vite HMR on :1430 + Tauri window
```

Or click **Threadknot (Dev)** in the app grid (`~/.local/bin/threadknot-dev-app`).
The dev backend needs :42800 too, so quit the release app first — the
launcher checks and refuses to start rather than kill it. Launcher log:
`~/.threadknot/dev-launcher.log`.

### Frontend-only changes: test them without rebuilding anything

The axum server serves `dist/` **from disk** (`resolve_dist()` in `server.rs`), so
after `npm run build` the already-running instance serves the new frontend
immediately — open `http://127.0.0.1:42800/?token=$(jq -r .token ~/.threadknot/server.json)`
in a browser (agents: the `threadknot-browser` MCP tools drive it, real CDP input, so
drag/click/hover behaviour is testable) and reload. Only the **Tauri webview**
needs the release rebuild, because it loads assets embedded in the binary.

Note the browser's `localStorage` is separate from the desktop webview's, so
per-device prefs (theme/zoom, sidebar layout, project order, unread markers)
start at their defaults there and testing them cannot disturb the live app.

### Headless (fastest to iterate on backend / smoke test — no GUI)

```bash
cargo build --bin threadknot-headless --manifest-path src-tauri/Cargo.toml
./src-tauri/target/debug/threadknot-headless      # prints local + LAN URL w/ token
```

## Ports, and running two instances side by side

One instance binds **three** sockets, derived from `THREADKNOT_PORT` (default
42800; also pinned in `server.json` once that file exists):

| port | what | who reaches it |
| --- | --- | --- |
| `<port>` | LAN/Tauri compatibility listener, plain HTTP | browsers, the webview, paired phones |
| `<port+1>` | strict remote ingress, loopback only | the connector process on this same machine |
| `<port+2>` | mesh listener, **TLS** | paired Threadknot machines |

`PROTOCOL.md` explains why these are three sockets and not one socket with three
policies. What matters here is that changing `THREADKNOT_PORT` moves all three,
so two instances need at least three ports of clearance between them.

> **GOTCHA — the 42800 range sits inside Linux's ephemeral port range, so a bind
> can fail with `Address already in use` when nothing is listening.**
> `ip_local_port_range` is `32768 60999` by default, and 42800/42801/42802 are
> inside it: any outbound socket on this machine may already hold one of them as
> its source port. It is intermittent, it looks like a stale process, and killing
> things does not fix it. Two test instances hit this the first time
> `mesh-smoke.py` was run. **Give test instances ports below 32768** (the smoke
> script uses 22810 and 22820 for exactly this reason). A dedicated server takes
> the other route and reserves its fixed ports with `ip_local_reserved_ports`,
> which is the right fix for a service but not something to demand of a
> developer's machine.

A second instance needs three things separated, not two:

```bash
HOME=/tmp/tk-b/home \
THREADKNOT_DATA_DIR=/tmp/tk-b/data \
THREADKNOT_PORT=22830 \
  ./src-tauri/target/debug/threadknot-headless
```

`HOME` is the one people forget. `THREADKNOT_DATA_DIR` does **not** isolate the
Library, which writes into the CLIs' own global skill directories
(`~/.claude/skills/` and friends) and removes the destination first — so a second
instance exercising installs will clobber the skills the live app is using.

`scripts/mesh-smoke.py` does all of the above and is the fastest way to check
mesh work end to end: two sandboxed headless instances, the two-phase pairing
handshake, and then assertions on the properties that matter rather than on "it
connected" — that a routed request crosses the link, that a device's grants are
enforced on the **far** side, that no credential or certificate reaches a
client-facing response, and that the mesh listener refuses both a master token
and a URL credential.

> **Never delete `mesh-ca.pem` to "reset" the mesh.** Every peer pinned that
> certificate authority at pairing, so removing it silently unpairs all of them —
> they stay listed, and every connection fails the handshake. `MeshIdentity`
> therefore reuses an existing identity always, and treats a corrupt one as a
> hard error rather than quietly re-minting. Re-pairing is the only repair.

## Verify gate (must pass before declaring done)

```bash
cd src-tauri && cargo build && cargo clippy      # Rust
cd ..        && npm run build                     # frontend typecheck + bundle
```

A change to `relay-protocol/` has a second gate that is **not in this
repository** — see [the wire contract](#the-wire-contract-relay-protocol) below.

Then a **behavioral** smoke test with a real agent turn (do NOT just trust compiles):
run `threadknot-headless`, grab the token from its stdout (or `~/.threadknot/server.json`),
and drive `/ws`. Pattern (Node ≥20 has a built-in `WebSocket`):

```bash
node - <<'EOF'
const t = require('fs').readFileSync(process.env.HOME+'/.threadknot/server.json','utf8');
const token = JSON.parse(t).token;
const ws = new WebSocket(`ws://127.0.0.1:42800/ws?token=${token}`);
let id=0; const req=(type,p={})=>ws.send(JSON.stringify({id:++id,type,payload:p}));
ws.onopen = ()=>req('hello');
ws.onmessage = m => { const v=JSON.parse(m.data); console.log(v.type, v.event?.kind ?? ''); };
EOF
```

A fuller script (project→thread→turn→approval round-trip) is worth recreating when
touching the drivers; see git history around the initial commits for the shape.

For the mid-thread agent-switch feature, two ready smoke scripts exist (recreate
from git history if /tmp was cleared): `/tmp/threadknot-smoke.mjs` (claude teaches a
codeword → `thread.setAgent` codex → codex recalls it) and
`/tmp/threadknot-smoke-roundtrip.mjs` (claude→codex→claude; proves native resume +
delta seed together). Run the headless server on a spare port first:
back up `~/.threadknot/server.json`, delete it, start with `THREADKNOT_PORT=42899`
(the file pins the port once written), and restore it afterwards. NOTE: the
desktop app usually holds :42800 — never kill it; and remember `pgrep/pkill -f
threadknot` matches your own shell's cmdline.

## Gotchas learned the hard way (all currently fixed — keep them fixed)

1. **`cargo build --release` ≠ shippable** — see rebuild section above.
2. **Wayland + NVIDIA webview crash.** Without `WEBKIT_DISABLE_DMABUF_RENDERER=1`
   the window dies instantly with `Error 71 (Protocol error) dispatching to Wayland
   display`. `main.rs` sets it at startup. **Do NOT also set
   `WEBKIT_DISABLE_COMPOSITING_MODE=1`** — that forces software rendering and the
   whole UI flickers/repaints constantly. One flag only.
3. **Desktop launches don't inherit your shell PATH.** Apps started from the icon get
   a minimal PATH, so `claude` (`~/.local/bin`), `codex` (nvm bin), and `kimi`
   (`~/.kimi-code/bin`) are invisible
   → "CLI not found" even though they work in a terminal. `agents/mod.rs::agent_path()`
   rebuilds a PATH covering `~/.local/bin`, `~/.kimi-code/bin`, `~/bin`, every
   `~/.nvm/versions/node/*/bin`, `~/.cargo/bin`, `~/.bun/bin`, `/usr/local/bin`.
   Drivers spawn via `resolve_bin()` and set `PATH` on the child. If an agent CLI
   lives elsewhere, extend `agent_path()`.
4. **Tauri v2 denies plugin commands without a capability.** `src-tauri/capabilities/
   default.json` must grant `dialog:default` (folder picker) and `core:default`.
   Symptom of a missing grant: the native "Add project" dialog silently does nothing.
5. **Agent auth is the CLIs' own subscription login** (`claude login` /
   `codex login` / `kimi login`).
   Never inject `ANTHROPIC_API_KEY`. Overriding `HOME` breaks Claude's OAuth keychain
   lookup — leave `HOME` alone; only `CLAUDE_CONFIG_DIR` / `CODEX_HOME` are safe to set
   for isolation (not currently used).
6. **chromiumoxide's `.arg()` takes a BARE switch — it adds the dashes itself.**
   `.arg("--disable-gpu")` renders `----disable-gpu`, which Chrome silently ignores
   (no warning, no error). Every explicit flag in `spawn_session` was a no-op this
   way, including the `--disable-features=IsolateOrigins,site-per-process` that the
   cross-origin-iframe support depends on. Write `.arg("disable-gpu")`, and for
   values use the tuple form `.arg(("user-agent", ua))` / `.arg(("disable-features",
   &[...][..]))` — tuples merge with chromiumoxide's same-key defaults instead of
   emitting a second switch that would drop them. Verify with
   `tr '\0' '\n' < /proc/<chrome-pid>/cmdline`; a `----` prefix is always a bug.
7. **Bot detection: `--headless=new` is the discriminator, and nothing else is.**
   Measured against Google search from this machine (2026-07-26): plain `curl` with
   a browser UA gets results, so the IP is fine — but *every* headless Chrome variant
   gets the `/sorry` reCAPTCHA interstitial, while headful Chrome on a real display
   passes. Tested and ruled out individually: `navigator.webdriver`, the
   `HeadlessChrome` UA token, `--enable-automation`, a cookie-warmed profile, and the
   GPU (forcing the real NVIDIA renderer with `--use-gl=angle --use-angle=gl` in
   headless is still blocked; headful with `--disable-gpu` and no WebGL at all still
   passes). `browser.rs` now fixes the cheap tells anyway — real `Chrome/<v>` UA
   derived from the binary, `--disable-blink-features=AutomationControlled`, and a
   `screenWidth/screenHeight` override (headless otherwise reports an 800x600 screen
   holding a 1280x800 window, which is impossible) — and those help on softer checks,
   but Google/Cloudflare need headful. The fix is Xvfb (`xorg-server-xvfb`): run
   headful inside a virtual display. Not implemented — it adds a system dependency
   and per-machine display plumbing.
8. **The desktop CSP needs one directive per media type you load from the local
   server.** `tauri.conf.json`'s `security.csp` has no `default-src` fallback that
   covers `http://127.0.0.1:42800` — every kind of subresource needs its own explicit
   loopback allowance. `img-src` had one, `media-src` did not, so `<video>`/`<audio>`
   artifacts silently failed to play in the Tauri webview (works fine in a phone
   browser, which loads the UI from that same origin and so hits `'self'`). Symptom:
   the player renders but never loads metadata. When adding a new artifact viewer,
   add the matching directive.
9. **The transcript scroller owns its position; don't let a rerender own it.**
   `ThreadView`'s layout effect must never write `scrollTop` while
   `state.feedThreadId !== state.activeThreadId` — `openThread` clears the feed and
   nulls `feedThreadId` before `thread.get` resolves, and writing
   `scrollTop = scrollHeight` against that empty feed pins the reused scroll node at
   the top. It also actively restores the reader's last position when they aren't
   following the bottom, because WebKit resets a nested scroller on unrelated
   rerenders (a completion toast from *another* chat was enough to jump the open
   chat to the top). Sticky-following resets only when a newly selected feed has
   actually loaded, not the moment the active thread changes.

10. **Four things made "loading log…" hang over HTTP — keep all four fixed.**
    Symptom: on a phone or in a browser (LAN or tunnel), opening any thread sat on
    `loading log…` for tens of seconds, even a brand-new thread with no events.
    All four are on the path between clicking a thread and painting its feed, and
    all four are measurable:
    - *Head-of-line blocking.* `handle_socket` used to `await handle_request(...)`
      inline in the read loop, so one socket handled one request at a time. A single
      `git.repos` shell-out or a request routed to a sluggish peer stalled every
      request queued behind it. Requests now run in `tokio::spawn` under a
      `MAX_INFLIGHT_REQUESTS` semaphore. Measured: `thread.get` returns in 23 ms
      alongside 8 concurrent git shell-outs that take 620 ms; before, it waited for
      them. Responses are correlated by id, and the client orders anything
      order-sensitive with its own awaits — don't re-serialize this loop.
    - *Catalog storm.* `Hub::emit` broadcast `("threads", None)` on every
      status-changing event, and an unscoped `threads` change makes the client run
      `refreshProjects()` — `project.list` + `workspace.list` (**152 KB**, it carries
      sidebar art) + a `thread.list` per project + the remote fan-out. That fires
      ~78x over one thread's life. It now names the owning project, so clients
      refresh one small thread list; `App.tsx` also coalesces those refreshes
      (`coalesced(200)`).
    - *Delta firehose.* Every socket received every event of every thread, including
      token-level deltas for threads that client isn't displaying — which the reducer
      discards (`seq < 0` only applies to the viewed thread) and `noticeBody` ignores.
      The writer now drops transient events for any thread but the one this socket
      last called `thread.get` on. Persisted events (`seq >= 0`) still always go out:
      they drive status, attention badges, and notifications.
    - *Uncompressed bytes.* No `CompressionLayer` meant the bundle shipped raw —
      1.22 MB of JS and 132 KB of CSS. With br: **319 KB and 27 KB**. Note axum has
      no permessage-deflate, so WebSocket frames are still uncompressed; that's why
      replay size (below) is handled by trimming rather than compression.
11. **Thread replay elides long tool output; the card fetches the rest on demand.**
    Tool output was 79% of a big thread's replay (2.0 MB of a 2.58 MB log). Those
    cards all render collapsed, so the bytes bought nothing while the whole log had
    to arrive before the feed painted. `trim_replay_output` (server.rs) keeps
    head+tail past a 2 KB cap and sets `ToolEnd.truncated`; expanding the card calls
    `thread.toolOutput` for the full text. Worst thread measured 2.58 MB → 0.96 MB.
    Two invariants: `truncated` is **replay-only** — never persist it, never set it
    on a live event (drivers pass `truncated: false`) — and the trim must round its
    cut points to char boundaries, or a multibyte output panics on slicing.
12. **`target="_blank"` is a no-op in the Tauri webview — links looked dead in the
    desktop app.** Every link the app renders (markdown links from an agent, the
    GitPane PR link, image previews) is a plain `<a target="_blank">`. Browsers open
    a tab; WebKitGTK asks Tauri to build a new window, nothing handles that, and the
    click is silently dropped. Fix: `tauri-plugin-opener` + one capture-phase click
    listener (`src/lib/links.ts`, installed from `App.tsx`) that intercepts anchors
    in the Tauri env only and hands the href to `openUrl`. Two carve-outs the
    listener must keep: skip `<a download>` (the Files/Artifacts panes save files by
    clicking a hidden one), and only act on explicit `https?:`/`mailto:`/`file:`
    hrefs so relative links stay app-internal. The mobile WebView already routes
    externals via `onShouldStartLoadWithRequest` → `Linking.openURL`; plain browsers
    are untouched.
13. **The mobile terminal key row typed the literal word "undefined" into CLI
    prompts.** Tapping ↑/↓ during `eas login`'s "Select a Team" filled the filter
    with `unuunundefineddefined…`. The buttons send the right bytes — the damage was
    the *focus churn* around them: a tap blurs the xterm textarea and `key()`
    refocuses it, so with DECSET **1004** (focus reporting) left on by an earlier
    program in that pty, every tap sent `\x1b[O` … `\x1b[A` … `\x1b[I`. Node's
    readline emits `keypress(undefined, {name: undefined})` for a complete-but-
    unnamed escape sequence (it passes `escaped ? undefined : s` as the string), and
    `prompts` — what eas/expo use — falls through to its typed-character handler,
    doing `input = s1 + c + s2` with `c === undefined`. Its cursor advances by 1 per
    insert, hence the interleaved garbage. Fix: `onMouseDown` → `preventDefault()` on
    the `.term-keys` toolbar, so focus (and the soft keyboard) never leaves the
    terminal. Desktop never hit it — real arrow keys don't move focus.

## Wire-protocol facts (verified against installed CLIs)

- Browser architecture, research decisions, tool contract, security boundaries,
  and its real-Chrome tests live in [`BROWSER.md`](BROWSER.md). Read its
  **signed-in profiles** section before touching `browser_profiles.rs` or the
  profile paths in `browser.rs`: those sessions hold real logins, and their
  guarantees (origin scope enforced in the browser, no `evaluate`, site
  isolation on, master-principal only, one thread at a time) are the reason the
  feature is safe to ship at all.
- **Claude** (`agents/claude.rs`): spawn `claude --output-format stream-json --verbose
  --input-format stream-json --include-partial-messages --permission-prompt-tool stdio
  --setting-sources user,project,local --model <id>`. No `--print`. Optional 1M
  context = `<id>[1m]` suffix (only `claude-fable-5`, `claude-sonnet-5`);
  `claude-opus-5` is always 1M and uses its unsuffixed canonical id. Permissions arrive as stdout
  `control_request`/`can_use_tool`; answer with a `control_response`
  `{behavior:"allow",updatedInput} | {behavior:"deny",message}`. Mid-session control:
  `{type:"control_request",request:{subtype:"interrupt"|"set_model"|"set_permission_mode"}}`.
  Plan mode = deny `ExitPlanMode` and, on approval, send `set_permission_mode` to the
  base mode. Access→permission-mode: read→(omit, "default"), edits→`acceptEdits`,
  full→`bypassPermissions`(+`--allow-dangerously-skip-permissions`); plan→`plan`.
  Threadknot always passes the `--allow-dangerously-skip-permissions` opt-in flag so
  a live session may later be promoted to full access; permissions are still
  enforced unless/until its mode is explicitly changed to `bypassPermissions`.
  Interrupting while `can_use_tool` is pending must deny/resolve that request
  before sending `interrupt`, otherwise the Claude process remains blocked on
  the permission response and the turn never reaches an aborted boundary.
  A live stdin pipe is not a liveness signal: one Claude HTTP connection can
  remain in TCP retransmit backoff while the CLI still answers local
  init/status/context-control frames. `FirstResponseWatchdog` therefore starts
  at each user message and only disarms on provider model/tool output. After 90
  seconds without that output it retires the child, repairs the saved transcript,
  resumes once on a fresh CLI process, and re-sends the same logical request.
  It never replays after model/tool output (which could duplicate mutations), and
  a second pre-response timeout fails explicitly instead of looping. Stop is
  also a process boundary: close the command receiver before emitting
  `turn_aborted`, briefly await the interrupted result, then retire the child so
  the next turn cannot reuse an unhealthy process.
  Context-window occupancy comes from the CLI's authoritative
  `get_context_usage` control request (`totalTokens`/`maxTokens`) plus resilient
  fallbacks: latest main-thread assistant/message-delta input-side usage and
  `compact_boundary.compact_metadata.post_tokens`. Emit `context_usage` whenever
  any of these changes; never take the maximum window from session-cumulative
  `modelUsage`. Model defaults evolve (a live 2.1.212 probe reported Fable 5 as
  1M even without a suffix), so the control response always wins over the
  static table.
  Opus 5 support landed in Claude Code 2.1.219. An older 2.1.212 client accepts
  the new model id and runs the turn, but its `get_context_usage` response still
  reports the legacy 200K ceiling; update Claude Code for accurate 1M context
  handling and metering.
  Claude effort defaults stay model metadata (`ModelInfo.default_effort`) for the
  composer's `Default (High)`-style label, but `ThreadSettings.effort = None` is
  the actual default. In that state the driver must omit `--effort` entirely;
  only a concrete user override may add the flag.
  Native Claude chats may opt into Claude in Chrome with
  `ThreadSettings.claude_chrome`; the driver adds `--chrome` at process launch.
  Changing it retires an idle child so the next turn resumes with the new flag.
  Never pass it to Claudex profiles.
- **Codex** (`agents/codex.rs`): spawn `codex app-server`. Newline-delimited JSON-RPC,
  **no `jsonrpc` field**, integer ids from 1. `initialize`→`initialized` notification→
  `thread/start`|`thread/resume`→`turn/start`. Approvals are server→client *requests*
  answered `{decision: accept|acceptForSession|decline|cancel}`. **`account/read` and
  `model/list` REQUIRE `params:{}` — omitting params errors "missing field params".**
  `model/list` response is under **`data`** (not `models`); skip `hidden:true`; efforts
  are `supportedReasoningEfforts[].reasoningEffort`. Access→(approvalPolicy,sandbox):
  read→(untrusted,read-only), edits→(on-request,workspace-write),
  full→(never,danger-full-access). Plan mode = `collaborationMode:{mode:"plan"}` on
  `turn/start`. Turn-level sandbox uses `sandboxPolicy:{type:...}` (object), not the
  thread-level string. Mid-turn user context uses `turn/steer` with the active
  `expectedTurnId`; if that precondition loses a boundary race, Threadknot preserves
  the note as a normal follow-up instead of interrupting or dropping it.
- **Kimi Code** (`agents/kimi.rs`): spawn `kimi acp`. This is ACP v1,
  newline-delimited JSON-RPC 2.0. Threadknot calls `initialize` → `authenticate`,
  then `session/new` or `session/resume` → `session/set_config_option` for
  model/thinking/mode → `session/prompt`. The prompt request stays open for the
  entire turn while `session/update` notifications stream text, thinking, tool
  calls, diffs, plans, and usage. `session/cancel` is a notification.
  `session/request_permission` is a reverse request; respond with
  `{outcome:{outcome:"selected",optionId}}`. Kimi's question tool uses that
  same request shape and currently supports one option-based question over ACP.
  Kimi's own TUI can steer through its private runtime, but ACP v1 advertises no
  steer method and rejects a second prompt while one is active. Threadknot therefore
  matches Kimi's Enter behavior: messages sent during a prompt queue locally and
  become `session/prompt` requests at provider-turn boundaries; Stop remains the
  separate `session/cancel` path.
  Access/mode mapping: plan→`plan`; build+read/build+edits→`default` with Threadknot
  auto-approving only edit/delete/move at edits access; build+full→`yolo`.
  Both K3 aliases advertise thinking efforts `low` / `high` / `max`, with
  `high` as the Kimi Code default; K2.7 aliases have no effort picker.
  ACP does not send a separate completion notification for narrated message
  chunks. Treat a content-kind change and the start of each new tool call as a
  hard segment boundary: persist the accumulated thought/message before the
  next segment or tool, rather than collecting the whole turn at the end.
  Empty thought chunks are separators and must not create blank feed items.
  Kimi's `Agent` tool likewise exposes only its launch input and final result
  over ACP. The CLI does, however, append the child's normalized activity to
  `~/.kimi-code/sessions/<workspace>/session_<id>/agents/agent-N/wire.jsonl`.
  Threadknot assigns `agent-N` from the session's existing max index and tails that
  append-only file while the call is open, translating child text/thinking/tool
  records into the same `subagent_*` events used by Claude. Missing files are a
  supported fallback (brief + elapsed timer still render); never make the turn
  depend on this diagnostic stream or assume it exists on a future CLI version.
  The browser MCP is passed to `session/new`/`session/resume` as an HTTP server
  with its thread bearer header. Images are native ACP `{type:"image", data,
  mimeType}` blocks; documents are materialized into the workspace like Codex.
  OAuth stays in the CLI and charges the Kimi membership quota — run
  `kimi login`; never add a Kimi API key to Threadknot.
- Regenerate codex schemas after a codex upgrade:
  `codex app-server generate-json-schema --out docs/protocol/`.
- **Driven browser** (`browser.rs`, `mcp.rs`, `BrowserPane.tsx`): one
  chromiumoxide/CDP Chrome per thread is shared by the agent's thread-scoped
  `/mcp` tools and the human `/browser` screencast socket. Keep this unified:
  never create a second "agent-only" page. Semantic accessibility snapshots
  assign document-scoped `eN` refs backed by CDP backend node ids; clear them on
  navigation and fail stale refs rather than guessing. Every MCP action is
  serialized through `action_lock` and emits privacy-filtered activity phases
  used for the visible cursor, target outline, HUD, and audit trail. `/browser`
  disconnect only detaches the viewer—the registry owns session lifetime.
  Frontend tab changes must therefore never close the socket; only unmount,
  server/thread change, or explicit `reset` may do so. Chrome profiles are
  per-session temporary directories. Keep upload canonicalized beneath the
  current project root; do not expose arbitrary host paths to page content.
  New page targets are wired lazily and become the shared active page; each
  active switch stops the old screencast, reapplies the emulated viewport, and
  starts the selected page's stream. Preserve the popup/target-created and
  target-destroyed listeners when changing lifecycle code.
  When extending the toolset, prefer compact semantic results, deterministic
  CDP actions, bounded waits, useful stale-state errors, and an optional
  post-action snapshot.
- **Mid-thread agent switching** (Traycer-style, see PROTOCOL.md "Mid-thread agent
  switching"): `thread.setAgent` flips `Thread.agent`; the next `turn.start` spawns
  the new provider's driver, resuming that provider's own session from
  `Thread.session_anchors` and prepending a handoff seed
  (`agents/transcript.rs::render`, everything past the anchor's
  `covered_until_seq`) to the first user message — a bare seed frame would itself
  start a turn on the claude stream-json channel. `covered_until_seq` advances on
  every turn_completed/turn_aborted in `Hub::emit`. Driver-exit cleanup checks
  `same_channel` before removing the sessions entry — an agent switch may already
  have registered a replacement driver for the same thread.
- **Restart recovery:** recovery starts only after the new server successfully
  binds Threadknot's port (never in `Hub::new`), so a failed second launch cannot
  interrupt or duplicate the live instance's work. Persisted `running`
  Claude/Codex turns get an explicit `turn_aborted` boundary followed by an
  automatic continuation turn against the provider's saved native session.
  `waiting_approval` / question turns are aborted but never auto-answered; their
  existing cards remain actionable through the stale-response recovery path.
  Hermes is different: its remote Runs API work survives locally, so
  `Thread.provider_run_id` is persisted immediately after submission and the new
  driver reattaches to that exact run/SSE stream instead of launching a duplicate.
  `turn.interrupt` still emits `turn_aborted` when no live driver accepts the
  command, preventing an orphaned thread from remaining stuck. On a Codex
  continuation, `thread/resume` must reactivate the id in the new app-server
  process before `turn/start`; if native history is unavailable, Threadknot starts a
  replacement thread and seeds it with the complete persisted transcript.
- **Usage meter** (`usage.rs`, see PROTOCOL.md "Provider usage"): Claude usage =
  GET `api.anthropic.com/api/oauth/usage` with the token from
  `~/.claude/.credentials.json` — passed to curl via `-K -` (config on stdin) so
  it never shows in argv; **this endpoint can 429 with multi-minute penalties**,
  keep the cadence conservative (15 min poll, 120 s kick floor). Codex usage =
  ephemeral `codex app-server` probe → `account/rateLimits/read` (params `{}`
  required, like all codex account calls); on Spencer's Pro plan `primary` is
  the WEEKLY window (windowDurationMins 10080) and `secondary` is null — don't
  assume primary=5h. Live codex sessions push `account/rateLimits/updated`
  (sparse — merge, don't clobber the plan) which `codex.rs` feeds back via
  `usage::publish`. Smoke: `/tmp/threadknot-usage-smoke.mjs` (node ≥22, native
  WebSocket, same port-override procedure as the other smokes).
- **Notifications** are client-side only (no wire change): `App.tsx::maybeNotify`
  on persisted turn_completed/error/approval_request/question_request, Traycer's
  presence rule (focused + viewing that exact thread → silence; focused on a
  different thread still gets a native alert). Desktop native path is
  the Tauri `notify` command (`notifications.rs`, notify-rust) because the WebKit webview has
  no Notification API; the LAN phone URL is plain http (insecure context) so
  browsers there get toast + chime + vibration only — that's why the in-app path
  always fires. Linux must send `desktop-entry=threadknot` so GNOME associates the
  request with `threadknot.desktop`; Windows must use an NSIS-installed build so
  `com.smithnetwork.threadknot` is registered as its AppUserModelID. Test the exact
  native path from Settings → **send test**, or without starting the GUI via
  `./src-tauri/target/release/threadknot --test-notification`. Web Notification API
  only works if Threadknot is ever served over HTTPS. Keep the Linux
  `notify-rust::NotificationHandle` alive: GNOME 50 watches the proxy-added
  `x-shell-sender` D-Bus name and immediately destroys the app's notification
  source when that sender vanishes. Dropping the handle reproduces the
  particularly misleading “GNOME returned an id, but no banner/history” bug.

## Adding things

- **New request type**: add to `PROTOCOL.md`, handle in `server.rs::handle_request`,
  add the typed call in `src/lib/protocol.ts` `RequestMap` + `src/lib/ws.ts`.
- **New agent event kind**: add to `protocol.rs::AgentEvent` (snake_case tag) AND
  `src/lib/protocol.ts::AgentEvent`, render it in `src/components/FeedItems.tsx`,
  fold it in `src/state/feed.ts`. Mark deltas transient in `AgentEvent::is_transient`.
- **New provider model / effort**: models flow live from the driver `probe()`; nothing
  hardcoded client-side. Claude's list is `agents/claude.rs::builtin_models()`.
- **Provider brand icons**: `src/components/icons.tsx` — `ClaudeMark`/`CodexMark` are
  the official single-path logos (from simpleicons.org), tinted via `.mark-claude` /
  `.mark-codex` `color:` in `styles.css` (`fill="currentColor"`).

## Claudex — the Claude harness on someone else's model

**What it is.** Claude Code is an excellent harness (tools, permissions, plan
mode, subagents, hooks, MCP) that happens to speak the Anthropic Messages API.
Point `ANTHROPIC_BASE_URL` at a local bridge that translates that API to
another provider's, and you keep the harness while another model does the
thinking. Anthropic documents this shape (LLM gateway configuration); it is not
a hack of the CLI. `Agent::Claudex` is that arrangement, and each **profile**
(`claudex.rs`, `~/.threadknot/claudex.json`) is one endpoint + model + environment.

**Why a separate agent kind rather than another Claude model.** Sessions, live
drivers, and session anchors are keyed by `Agent`. Sharing the kind would let a
thread reuse a running, genuinely Anthropic-authenticated `claude` process after
the user picked a bridged model — the user's real plan, silently billed. The
enum split makes that unrepresentable, and `agents/mod.rs::session_key` extends
the same logic one level down so a *profile* switch also forces a respawn.

**What Threadknot does NOT do.** It does not implement the Anthropic⇄provider
translation. That is a large, moving surface (SSE shape, tool blocks, thinking
blocks, images, count_tokens, compaction) and a maintained project already does
it. Threadknot spawns/supervises the bridge and owns everything above it.

If you are ever tempted to write the translator here, `codex app-server` is the
seductive wrong answer. It cannot work, for one structural reason: app-server is
**stateful** and the Anthropic Messages API is **stateless**. `TurnStartParams.input`
is a `Vec<UserInput>` (Text / Image / Audio / Skill / Mention) with no way to
submit an assistant turn or a prior `tool_use`/`tool_result` pair — Codex owns
the transcript inside the thread, while Claude Code POSTs the entire conversation
on every request. The only injection hatch is `ThreadResumeParams.history`, which
upstream annotates `[UNSTABLE] FOR CODEX CLOUD - DO NOT USE`.

**Setup (the intended default: Codex on a ChatGPT subscription).**

```sh
brew install raine/claude-code-proxy/claude-code-proxy   # or the release installer
claude-code-proxy codex auth login                       # ChatGPT Plus/Pro, NOT an API key
```

Then Settings → agents → **claudex profiles** → add. The form is pre-filled with
the working configuration: `http://127.0.0.1:18765`, model `gpt-5.6-sol`, small
model `gpt-5.6-luna`, sidecar `claude-code-proxy serve`. Leave **context window**
blank and the server reads it from the provider's own per-account catalog.
Press **test** to make Threadknot start the bridge and confirm it answers.

**Per-profile facts worth knowing.**
- **`CLAUDE_CONFIG_DIR` is per profile** (`~/.threadknot/claudex/<id>/`). Bridged
  transcripts, session ids and settings never touch `~/.claude`, so a bridged
  thread can't resume — or corrupt — a real Claude session.
- **`ANTHROPIC_API_KEY` is blanked** in the child. An inherited key would
  outrank `ANTHROPIC_AUTH_TOKEN` and send the traffic to Anthropic on a real
  API account.
- **No `[1m]` suffix.** It is a Claude-client hint and widens nothing upstream.
- **The window is a real setting, not a label** — it becomes
  `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, so getting it wrong either wastes context
  or lets a turn run past the limit and fail mid-work. Leave it blank and
  `claudex::catalog_window` reads `~/.codex/models_cache.json`, the Codex CLI's
  own per-account catalog, and returns the **effective** window
  (`context_window` × `effective_context_window_percent`).
- **`gpt-5.6-sol` is 272 000 here, not the 1.05M you will read elsewhere.** That
  1.05M is the *API* window. The Codex product serves Sol at
  `context_window: 272000` with `max_context_window: 272000` — the ceiling, not
  a current setting, so it is not one config change away. (Contrast `gpt-5.4`,
  whose `max_context_window` genuinely is 1000000.) Codex then accepts 95% of
  it, so the usable figure is **258 400**. Do not "fix" this upward: the
  request is rejected long before 1M and the failure lands mid-turn.
- **Sidecars are loopback-only, enforced in `claudex.rs`.** A bridge listener is
  unauthenticated and holds a subscription; Threadknot itself binds the LAN, so a
  routable bridge would hand the subscription to the whole network.
- **Usage meters do not cover it.** The sidebar meter reads Claude and Codex
  credentials directly; a bridged turn spends whatever the bridge is signed
  into. For the Codex bridge the existing Codex meter is the honest one.
- **Token counts are estimates.** Bridges compute them locally, so the context
  ring is approximate on Claudex threads.

**Account risk, stated plainly.** OpenAI has publicly welcomed Codex through
other harnesses (and ships `openai/codex-plugin-cc` itself), but no vendor
guarantees future treatment of subscription traffic through a third-party
client. What Anthropic enforced against in 2026 was the *reverse* direction —
third-party clients using Claude subscription OAuth — which this is not:
`ANTHROPIC_AUTH_TOKEN` here is the bridge's, and
`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` keeps the bridged CLI from
phoning home.

**Smoke test without installing a bridge.** Run a stub HTTP server on
127.0.0.1:18765 that logs requests, point a profile at it, and drive one turn:
the main call must arrive as the profile's `model` with `stream: true`, the
background calls as `smallModel`, the auth header as the profile token, and
`~/.threadknot/claudex/<id>/{projects,sessions}` must exist. That proves the
env overlay and the isolation without needing a real subscription.

## Per-project solo windows (drag a project out of the sidebar)

Each project can live in its own OS window (one per GNOME workspace). Entry
points in the fleet window's sidebar: **right-click a project row → "Break out
into its own window"** (in-app `ContextMenu.tsx`, portal-rendered — one code
path on every OS and in plain browsers, no native-menu capabilities), hover the
row → pop-out icon, or drag the row onto the drop zone that covers the main
pane. True Chrome-style tear-off (window tracking the cursor mid-drag) is
impossible on Wayland (clients can't position windows), hence the drop zone;
everything here is plain cross-platform Tauri and works unchanged on
Windows/macOS.

Mechanics:
- Rust `open_project_window` command (lib.rs) creates/focuses a webview window
  labeled `project-<uuid>`; the title is the project name. The **label is the
  channel** that tells the frontend which project the window owns —
  `src/lib/solo.ts::detectSoloProject` reads it (query-param URLs don't survive
  `WebviewUrl::App`, so don't try). Plain browsers use `?project=<id>` instead
  (`openProjectWindow` falls back to `window.open`).
- `capabilities/default.json` must list `project-*` in `windows` or the new
  windows silently lose `server_info`/`notify`/dialog invokes.
- Solo windows are ordinary extra `/ws` clients; state stays server-authoritative.
  `state.solo` filters the sidebar and hides add/remove/schedules.
- localStorage is shared across windows: last-open-thread restore is keyed
  `threadknot.lastThread.<projectId>` in solo (`store.tsx::lastThreadKey`), and solo
  windows heartbeat `threadknot.soloWindow.<projectId>` so the fleet window
  suppresses duplicate notifications for projects that have their own window
  (`solo.ts::advertiseSoloWindow`/`hasSoloWindow`).

## Scheduled runs

`schedules.rs` fires recurring agent turns (Codex-automation style, run locally).
Cadence presets only (hourly / daily / weekdays / weekly at a local time — see
PROTOCOL.md), never cron. The loop follows the `usage::spawn_poller` shape: tick
every 30 s + a `hub.sched.kick` Notify poked by the `schedule.*` handlers. Each
firing calls `store.create_thread` + `hub.start_turn` (fresh thread per run, so
`start_turn`'s idle check can never block), then retitles the thread to
"`<name>` · Jul 21, 09:00". Missed-run policy: fire if ≤ 60 min late, else skip
with a note in `lastError` and roll forward — deliberate, so a machine that was
off for days doesn't stampede runs at boot. Recurrence math lives in
`next_occurrence` (unit-tested in the same file; keep `src/lib/schedule.ts`'s
mirror in sync — it powers the form's live preview). UI: `SchedulesPanel.tsx`,
opened from the sidebar's "scheduled runs" row.

## The Library — skills & MCP servers (`library.rs`, `catalog.rs`)

Settings → Library. Two shelves, two completely different mechanisms; conflating
them is the easy mistake.

**Skills are folders the CLIs find by themselves.** Threadknot does not inject them —
it manages directories the agents already scan:

| agent | user skills directory | how it discovers them |
| --- | --- | --- |
| Claude | `~/.claude/skills/<name>/SKILL.md` | because the driver passes `--setting-sources user,project,local` |
| Codex | `~/.codex/skills/<name>/SKILL.md` | scanned unconditionally |
| Kimi | `~/.kimi-code/skills/<name>/SKILL.md` | scanned unconditionally (`--skills-dir` overrides) |

All three were **verified empirically** (2026-08-06) by planting a probe skill in
each directory and asking the CLI to read a phrase out of its frontmatter. Kimi's
directory does not exist until something creates it; `install_skill` does.
Consequence worth remembering: a skill installed through Threadknot also works in a
plain terminal session of that CLI, because the CLI — not Threadknot — is what loads
it.

The lister also folds in **Claude Code plugin skills** from
`~/.claude/plugins/cache/<marketplace>/<plugin>/<version>/skills/<name>/` as
read-only rows (`removable: false`). When a plugin and a skills directory share a
name, it stays one row and sets `alsoFromPlugin` — otherwise removing your copy
looks broken when Claude keeps finding the plugin's.

**MCP servers are the opposite: nothing on disk discovers them.** The registry
(`~/.threadknot/mcp-servers.json`) is the only source of truth and every local driver
injects the enabled entries at spawn, alongside Threadknot's own browser server —
which the registry protects by refusing the name `threadknot-browser`. Per-driver
wire format, all three **verified against the installed CLIs** by asking each one
to name its `deepwiki` tools:

- **Claude** — merged into the inline `--mcp-config` JSON:
  `{"mcpServers":{"<name>":{"type":"stdio","command","args","env"}}}` or
  `{"type":"http","url","headers"}`.
- **Codex** — repeated `-c mcp_servers.<name>.<key>=<toml>` before the
  `app-server` subcommand. Values are parsed as TOML, so strings are JSON-quoted
  and args use array syntax. **Codex's HTTP transport has no arbitrary headers**:
  it takes `bearer_token_env_var`, so an `Authorization: Bearer …` header is
  translated into a per-server env var and anything else is dropped with a
  `tracing::warn!` (`codex_unsupported_headers`).
- **Kimi** — the ACP `session/new` `mcpServers` array. ACP carries `env` and
  `headers` as `{name, value}` pair arrays, **not** objects. `initialize` reports
  `mcpCapabilities: {http: true, sse: true}`.

Changes take effect for chats started from then on; a running session keeps what
it launched with.

**Gating.** All `library.*` mutations are master-token only (`server.rs`), the
same rule as browser profiles: an MCP server is code launched with Threadknot's
privileges and often a credential it can spend. `library.list` stays open so a
phone can read the shelf. The whole family is routable on `machineId`, and the
master gate runs *before* routing so a device credential cannot launder an
install through a peer.

**Licensing (do not regress this).** Catalog skills are pointers, fetched from
GitHub at install time — nothing of someone else's is vendored into the binary.
Anthropic's `docx`/`pdf`/`pptx`/`xlsx` skills are **source-available, not open
source**: their `LICENSE.txt` forbids retaining copies outside Anthropic's
services *and forbids derivative works*, so `LICENSE_BLOCKED` refuses them with a
message pointing at `claude plugin install document-skills@anthropic-agent-skills`.
Every other skill in `anthropics/skills` carries a real Apache-2.0 `LICENSE.txt`
in its own folder; that per-folder check is the rule for adding a catalog entry,
and the license file is downloaded along with the skill.

Community "MIT-licensed" copies of those four exist and **must not be used** —
most are verbatim relicensed copies of Anthropic's (several still carry
`license: Proprietary` in their own frontmatter), and a copier cannot grant MIT
over work that is not theirs.

**Threadknot's own document skills (`threadknot/skills/`, embedded via `bundled.rs`).**
Because the gap above is exactly the capability people come looking for, Threadknot
ships four **clean-room** replacements — `docx`, `xlsx`, `pptx`, `pdf` — written
against the public documentation of python-docx, openpyxl, python-pptx, pypdf,
pdfplumber and reportlab (all MIT or BSD) and the file-format specs. Anthropic's
skills were never read while writing them. Copyright covers their expression,
not the capability, so an independent implementation is clear — but that is only
true while it stays independent: **never consult their SKILL.md when editing
ours.** Ours are Apache-2.0, Copyright Smith Network Solutions.

These install from the binary with no network (`install_bundled_skill`), which is
also why they work offline and cannot half-install. Their scripts get the +x bit
explicitly — `include_str!` cannot carry file modes, and GitHub installs read it
from the tree API's `mode` field. Each script declares its dependencies inline
(PEP 723) behind a `uv run --script` shebang, so nothing needs installing first;
the SKILL.md files tell the agent to do the same for scripts it writes, because
a modern distro's system Python is PEP-668 managed and `pip install` fails.

Adding a file to a bundled skill means adding an `include_str!` line in
`bundled.rs` — deliberately explicit rather than a build-script directory walk,
so what ships in the binary is greppable. `bundled.rs`'s tests assert every
blocked upstream skill has a replacement.

Installs are plain HTTPS: one GitHub trees call, one raw fetch per file, staged
in a temp directory and published only once complete. Nothing executes during an
install. Guards: `safe_join` refuses any path escaping the skill folder, and
`MAX_SKILL_FILES`/`MAX_SKILL_BYTES` stop someone pointing it at a repository.

## The wire contract (`relay-protocol/`)

Everything in this repository works with no account and no internet. The
**hosted relay** — the optional paid tier that gives an installation a stable
public HTTPS origin — is a separate, private service, and it is not here. What is
here is the client half: `connector.rs`, which dials out to it, and
`relay-protocol/`, the contract the two speak.

`relay-protocol/` is a **standalone crate on purpose**, and its manifest carries
an empty `[workspace]` table for that reason. It is a path dependency of
`src-tauri` *and* of the service, which is a different workspace entirely — and a
crate that belongs to one workspace cannot be path-depended on from another.
Someone will eventually try to tidy it into a member of `src-tauri`; it will not
build.

There is deliberately **one** copy of it. Two definitions of "what gets signed"
is the pair that drifts silently, and the failure that drift produces — a
signature that verifies on one side and not the other — is indistinguishable from
a network fault. So changing a signed message layout, a frame, or a DTO in here
is a change to **both** ends at once, and the service adopts it as a deliberate
step rather than by tracking this branch.

Threat model and release gate for remote access: **`docs/REMOTE-ACCESS-SECURITY.md`**.

### Connector escape hatches (test only)

`connector.rs` reads four environment variables so it can be pointed at a relay
that is not production. **None of them may be set in a shipped configuration** —
each one moves where this machine's tunnel terminates:

| variable | what it overrides | why it exists |
| --- | --- | --- |
| `THREADKNOT_RELAY_HOST` | the host the connector dials (default: `relay_protocol::CONNECTOR_SNI`) | point a dev build at a test box |
| `THREADKNOT_RELAY_PORT` | that host's port (default 443) | a test relay on an unprivileged port |
| `THREADKNOT_RELAY_SNI` | the SNI presented (default: the same constant) | the relay routes on SNI, and it is **not** always the host being dialled — dialling a test box while still presenting the production SNI is what makes the connection land on the connector endpoint rather than on the data plane |
| `THREADKNOT_CONTROL_URL` | the control-plane base URL used by `connector.enroll` | enroll against a local control plane |

## State on disk

`~/.threadknot/`: `server.json` (`{port,token}` — delete to rotate token), `projects.json`
(project+thread+schedule index), `threads/<id>.jsonl` (append-only event log),
`claudex.json` (Claudex profiles — holds bridge tokens, same trust level as
`server.json`) + `claudex/<profileId>/` (each profile's isolated
`CLAUDE_CONFIG_DIR`, erased when the profile is removed — it holds that
profile's transcripts and nothing else can reach them), `dictation.json`
(voice provider settings and its write-only API key, mode `0600` on Unix), `mcp-servers.json`
(installed MCP servers — holds their tokens, same trust level as `server.json`;
skills live in the CLIs' own directories, not here). Env overrides: `THREADKNOT_PORT`, `THREADKNOT_DATA_DIR` (whole
store — use it for smoke tests), `THREADKNOT_DIST` (UI location), `RUST_LOG`.

**Mesh and connector identity** live in the same directory:

| file | secret? | notes |
| --- | --- | --- |
| `mesh-ca.pem` | no | this machine's self-signed mesh CA — the thing every peer pins at pairing |
| `mesh-ca.key` | **yes, `0600`** | signs the leaf; permissions are re-applied on every open, not just at creation, because a file restored from a backup arrives world-readable |
| `mesh-leaf.pem` | no | the leaf the mesh TLS listener serves, SAN `<machineId>.threadknot.mesh` |
| `mesh-leaf.key` | **yes, `0600`** | the listener's private key |
| `peers.json` | **yes, `0600`** | per-pair credentials, inbound credential hashes, each peer's pinned CA |
| `connector.json` | no | server-assigned installation id + hostname, and the on/off flag |
| `connector.key` | **yes, `0600`** | the installation's Ed25519 identity, base64 seed |

Two of these are load-bearing beyond their contents. **Deleting `mesh-ca.pem`
unpairs every peer** (they pinned it — see "Ports, and running two instances side
by side"), and
regenerating `connector.key` orphans the installation, because the control plane
knows that key; rotation is an explicit two-call operation against the control
plane, never a side effect of a missing file.
