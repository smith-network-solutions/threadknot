<p align="center">
  <img src="docs/media/threadknot-wordmark.png" width="520" alt="Threadknot">
</p>

<p align="center"><strong>Every coding agent on one thread.</strong></p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="license: Apache-2.0"></a>
  <img src="https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows-lightgrey" alt="platforms">
  <img src="https://img.shields.io/badge/rust-stable-orange" alt="rust: stable">
</p>

A Tauri (Rust) desktop app that drives Claude Code, OpenAI Codex, and Kimi Code
natively over their wire protocols — no terminal wrapping, no Node server — and
serves the same UI to any browser on your LAN.

Pair your machines and they become one mesh: launch a thread on your desktop
from your laptop, watch three agents work on three different computers at once,
and drive the whole thing from your phone. No cloud, no account — your machines
talk to each other directly.

<p align="center">
  <img src="docs/media/hero-phone.gif" width="300"
       alt="A live agent turn streaming into the Threadknot UI at phone width — tool calls arriving one by one, a working indicator, and a composer that can interrupt the run.">
  <br>
  <em>The same UI a phone gets, streaming a live agent turn over the LAN.<br>
  (That's Threadknot working on Threadknot.)</em>
</p>

## Install

There are no prebuilt downloads yet. Build it — with Rust and Node 22+ present,
that's two commands:

```bash
npm install
npx tauri build --no-bundle
./src-tauri/target/release/threadknot
```

The same build produces `threadknot-headless`, the LAN server with no desktop
window, which is what you want on a spare machine or a homelab box.

You also need at least one agent CLI installed and already logged in, plus your
platform's webview toolchain — both under [Prerequisites](#prerequisites). More
build detail, including the one mistake that produces a binary with no UI, is
under [Build & run](#build--run).

Packaged `.deb`/`.rpm`/installer downloads will appear on
[Releases](https://github.com/smith-network-solutions/threadknot/releases) once
there's a tagged version; the workflow that builds them is in the repo.

## One phone, every machine

Pair two or more machines and they form a **symmetric mesh** — no hub, no cloud,
no account. Whichever Threadknot you happen to be looking at drives all of them,
and that includes the one in your phone's browser.

```
        phone · laptop · any browser on the LAN
                        │
             connect to ANY one machine…
                        ▼
   ┌───────────────┐         ┌───────────────┐
   │    desktop    │◀───────▶│    macbook    │
   │   claude ▶▶   │  peer   │   codex  ▶▶   │
   └───────┬───────┘         └───────┬───────┘
           │                         │
      peer │      ┌───────────────┐  │ peer
           └─────▶│    homelab    │◀─┘
                  │    kimi ▶▶    │
                  └───────────────┘

          …and you are driving all three.
```

A **workspace** groups threads that live on different machines under a single
sidebar entry. Starting a thread means picking which machine, and which of that
machine's registered folders, it runs in. From then on the thread is pinned
there — and its events stream to every paired device.

```
Workspace "Storefront"
 ├─ desktop   ~/projects/storefront        ← Claude, mid-refactor
 ├─ macbook   ~/work/storefront-seo        ← Codex, running tests
 └─ homelab   /srv/storefront              ← Kimi, on a long build
```

Three agents, on three machines, working at once. Open a new thread on any of
them, or interrupt a turn you don't like, from whichever device is in your hand.

- **Discovery is mDNS plus explicit pairing.** Identity is the machine id, so a
  DHCP lease change never breaks the mesh; IP addresses are disposable hints.
- **No cloud plane.** Peers talk directly over your LAN or tailnet. There is no
  sync service, no account, and nothing to sign up for.
- **Nothing is copied around.** A thread is pinned to the machine that owns it
  and runs against that machine's real filesystem. Git stays the only channel
  for code.

Still machine-local: git panes, terminals, and artifact bytes are served only by
the machine owning the thread, and push notifications don't yet fire for remote
threads. Details and the remaining work: [`docs/MULTI-MACHINE.md`](docs/MULTI-MACHINE.md).

## Make them argue

Agents are confidently wrong in ways another model often catches immediately.
So point one at another one's work.

**Review with…** throws one or more reviewers at a live thread. Claude planned
a refactor; hand the plan to Codex and let it argue. Reviewers are read-only —
they cannot touch a file, only make a case — and any agent can review, including
the one that did the work.

```
 thread: "Rewrite the sync layer"

   Claude  ── plans the refactor ──▶
                                     Codex    ── "this drops writes on
                                                  reconnect; here's the case"
                                     Kimi     ── "agreed, and the retry is
                                                  unbounded"
   Claude  ── concedes, revises ──▶
                                     …until everyone concedes, or you call it
```

One reviewer at one round is a plain critique. Add reviewers, or raise the round
count, and it becomes a debate that runs until the participants stop objecting.
They can be different providers, different models, or the same model twice —
each with its own model, effort, and access.

Design notes and the reasoning behind the roles: [`docs/PARLEY.md`](docs/PARLEY.md).

## Controls (per thread)

| Control | Options |
|---|---|
| Agent | Claude Code, Codex, Kimi Code, Claudex (Claude harness + any model) |
| Model | Claude: Fable 5 / Opus 5 / Sonnet 5 / Haiku 4.5 · Codex: discovered via `model/list` · Kimi: K3 / K3 256K / K2.7 Code |
| Effort | K3: low / high / max (high default) · other models expose their supported levels (+ **1M context** toggle on supported Claude models via the `[1m]` suffix) |
| Access | **Read-only** (ask for everything) / **Edits** (auto-accept edits) / **Full** (no prompts) |
| Mode | **Plan** (read-only planning, plan approval card → one-click "Approve & build") / **Build** |

## How it works

```
┌───────────────────────────────────────────────┐
│ Tauri shell (desktop window)                  │
│   └─ React UI  ←────────────┐                 │
├─────────────────────────────┼─────────────────┤
│ Rust core (threadknot_lib)  │  same UI, phone │
│   axum server :42800 ───────┴──── browser ────┼──→ http://<lan-ip>:42800/?token=…
│   ├─ /ws  (token-gated JSON protocol)         │
│   ├─ /browser (shared Chrome screencast)      │
│   ├─ /mcp (agent browser + artifacts)         │
│   ├─ agent hub (events → JSONL + fanout)      │
│   ├─ claude driver ── spawns `claude`         │   stream-json + control_request
│   ├─ codex driver ─── spawns `codex`          │   app-server (JSON-RPC/stdio)
│   ├─ kimi driver ──── spawns `kimi acp`       │   ACP (JSON-RPC/stdio)
│   └─ claudex ──────── `claude` + gateway env  │   any model, Claude harness
└───────────────────────────────────────────────┘
```

- **Auth is your existing subscriptions**: the local drivers spawn the
  installed `claude`, `codex`, and `kimi` CLIs, which use their own
  `claude login` / `codex login` / `kimi login` credentials. No API keys for
  the local agents. (Claudex is the exception: it runs the same `claude`
  harness against a compatible gateway, so it takes a base URL and key.)
- **Projects are folders**; each thread runs one agent in that folder with its
  own model / effort / access / plan-build settings.
- **Everything is event-sourced**: normalized agent events are appended to
  `~/.threadknot/threads/<id>.jsonl` and broadcast to every connected client, so
  desktop and phone stay in sync and threads replay on reconnect.
- Provider sessions resume: Claude via `--resume <session_id>`, Codex via
  `thread/resume`, and Kimi via ACP `session/resume`.
- **Agent and human share one browser:** the agent gets semantic page snapshots
  and deterministic browser tools while the Browser workspace shows the same
  live Chrome, including the agent cursor, target, action status, and failures.
  You can take over its mouse and keyboard at any time.

Protocol contract: [`docs/PROTOCOL.md`](docs/PROTOCOL.md). Codex app-server
schemas vendored in [`docs/protocol/`](docs/protocol/).

## Prerequisites

- **Rust** (stable) and **Node 22+**.
- **Platform toolchain for Tauri's webview stack.** On Linux:
  ```bash
  sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
      librsvg2-dev libgtk-3-dev libssl-dev patchelf
  ```
  macOS needs the Xcode command-line tools; Windows needs the MSVC build
  tools. Linux is the primary development target and gets the most testing.
- **The agent CLIs you intend to drive**, installed and *already logged in*:
  `claude`, `codex`, `kimi`. Threadknot does not handle auth — it reuses each
  CLI's own subscription login, so every person runs against their own account.
  An agent you are not logged into shows as unavailable in the composer;
  nothing else breaks.

The first build compiles the full dependency tree and takes a while. Later
builds are incremental.

## Develop

```bash
npm install
npm run tauri dev          # desktop app w/ vite HMR (port 1430)
```

## Build & run

```bash
npx tauri build --no-bundle                      # desktop app
./src-tauri/target/release/threadknot            # desktop app
./src-tauri/target/release/threadknot-headless   # LAN server only, prints URL
```

> Build the desktop app with the **Tauri CLI**, as above. A plain
> `cargo build --release` produces a binary with no UI embedded: it tries to
> load the dev server and opens on "Could not connect to localhost".
> `npx tauri build` runs `npm run build` for you.

Server state: `~/.threadknot/` (`server.json` holds the port + access token —
delete it to rotate the token). Override port with `THREADKNOT_PORT`, UI location
with `THREADKNOT_DIST`.

Upgrading from Armada? Nothing is migrated: an existing `~/.armada` keeps being
used, in place. See "Rename compatibility" in `CLAUDE.md`.

## Phone access

Open Settings (gear, bottom of sidebar) in the desktop app and open/copy the
LAN URL — e.g. `http://192.168.0.54:42800/?token=…` — on any device on your
network. The token persists in the browser after first load.

## Credits

Inspired by [t3code](https://github.com/pingdotgg/t3code) and
[Codex](https://github.com/openai/codex) — thanks to both teams.

t3code is MIT-licensed, © 2026 T3 Tools Inc.; its notice is in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

## License

[**Apache License 2.0**](LICENSE). Use it for anything — personally, at work,
commercially — modify it, fork it, redistribute it. Keep the copyright notice
and license text if you redistribute it, modified or not.

The hosted relay that backs the optional paid tier is a separate service and is
not in this repository. Nothing here needs it: the LAN server, the mesh, and the
mobile companion all work with no account and no internet.
