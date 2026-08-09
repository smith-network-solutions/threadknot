# Multi-machine Threadknot — implementation plan

> Status: PHASES 0–4 SHIPPED (2026-07-22). Identity, migration, workspaces,
> peering, mDNS/DHCP recovery, cross-machine workspaces (attach/detach/
> replicate/LWW-rename) and remote threads (machineId-routed RPCs + origin-
> tagged event relay) are implemented and smoke-tested — including a REAL
> claude turn created on instance B and driven/streamed from instance A,
> interrupt, rename, and the mirror direction. Phase 5 (hardening: artifact
> byte proxy, remote header persistence for offline rendering, push-for-
> remote, remote git/terminals) remains. Read alongside `PROTOCOL.md` (its
> "Mesh" section) and `DEVELOPMENT.md`.

## Goal

A **workspace** (e.g. "ServiceStorm") is a renameable container that groups
threads running on different machines under one sidebar element. Starting a
thread inside a workspace means picking **which machine** and **which of that
machine's registered root folders** it runs in; after creation the thread is
**pinned to that machine forever**. All of a workspace's threads — local,
remote, or driven by a remote Hermes agent — list together in the sidebar, and
you click between them exactly like today.

Hard requirements:

1. **Symmetric mesh.** Every paired machine can see and control every other —
   no dedicated hub. Whichever Threadknot you're sitting at (desktop or phone via
   its server) drives the whole mesh.
2. **Every existing thread gets this machine's id at migration.** No implicit
   "no id = local": a thread record must name its owning machine absolutely.
3. **DHCP-proof peering.** Identity is the machine id; IPs are disposable
   hints kept fresh by announcement + local (mDNS) discovery.
4. **Backwards compatible.** An existing single-machine `~/.threadknot` loads
   unchanged; with zero peers everything behaves exactly as today.
5. **Roots only.** The new-thread picker offers a machine's registered root
   folders for the workspace — never a browse into subfolders.

## Non-goals

- No cloud plane, accounts, or sync service (the Oz branch's dormant `mesh/`
  scaffolding beyond device identity stays unused).
- No thread migration between machines; pinning is permanent.
- No source-code sync — Git remains the only code channel.
- No WAN NAT traversal. Peers must be mutually reachable (LAN or tailnet IP).

---

## Entity model — Workspace ABOVE Project (Project unchanged)

Today a `Project` is literally "a folder on this machine" — and it stays
exactly that. We do NOT overload it. The new cross-machine container sits
above it:

```
Workspace "Storefront"              ← renameable, sidebar element, mesh-wide
 ├─ root: Project A (this machine, ~/projects/storefront)
 ├─ root: Project B (Mac,          ~/work/storefront-seo)
 └─ threads: every thread of A and B, each pinned to its machine
```

- `Workspace { id, name, createdAt, updatedAt, members: [{machineId,
  projectId}] }` — the FULL catalog replicates to every paired machine
  (pairing alone makes all workspaces visible everywhere; remote-only ones
  route through their owner). Edits reconcile last-write-wins by
  `updatedAt`; deletes via tombstones (`mesh.workspaceDelete`, re-pushed at
  every resync so an offline window can't resurrect them).
- `Project` keeps its exact current shape `{id, name, path, createdAt}` and
  remains machine-local. A project = one root folder on one machine. Repos,
  terminals, artifacts, git panes all stay project-scoped and machine-local.
- `Thread` gains `machineId` (REQUIRED after migration, immutable). Threads
  keep `project_id`; the workspace lists threads via its member projects.
- Hermes-agent threads need nothing special: a thread's *agent* may be a
  remote Hermes gateway while the thread itself still lives in some machine's
  project — it shows in the workspace like any other thread.

**Why this shape is the backwards-compatible one:** no existing field changes
meaning. Migration just wraps each existing project in a workspace of the same
name — and reuses the project's id as the workspace id, so anything keyed on
ids stays stable.

## Identity

- **`machineId` = the existing `server_id`** from `~/.threadknot/server.json`
  (stable, survives token/port changes, mobile push already keys on it). Do
  NOT mint a second UUID.
- New `~/.threadknot/device.json`: `friendlyName` (defaults to hostname — via
  `gethostname`/`/etc/hostname`, NOT `$HOSTNAME`) + detected capabilities
  (`run-claude`, `run-codex`, …), adapted from the Oz branch's
  `mesh/device.rs`.
- `device.info` RPC returns `{machineId, friendlyName, hostname, os, arch,
  version, capabilities}` — used in pairing and the machine picker.
- `meshVersion` int rides in `device.info` + peer handshake; mismatches refuse
  to pair with "update Threadknot on X".

## Storage & migration

```
~/.threadknot/projects.json    gains: workspaces: [Workspace], thread.machineId
~/.threadknot/device.json      NEW    (friendlyName, capabilities)
~/.threadknot/peers.json       NEW    peer registry (0600, hermes.json pattern)
   [ { machineId, name, port, meshPort, meshCa, addresses: [...],
       outboundCredential, inboundCredentialHash,
       lastGoodAddress, lastSeenAt, addedAt, meshVersion } ]
~/.threadknot/mesh-ca.pem      NEW    this machine's mesh certificate authority
~/.threadknot/mesh-ca.key      NEW    0600
~/.threadknot/mesh-leaf.pem    NEW    the leaf that CA signs
~/.threadknot/mesh-leaf.key    NEW    0600
```

**Migration (once, on first load of the new build):**
1. Stamp every thread's `machineId` with the local machine id; flush. (This
   machine: 41 threads.)
2. For each project, create `Workspace{ id: project.id, name: project.name,
   members: [{machineId: local, projectId: project.id}] }`.
3. Create `device.json` with `machineId := server_id`.
Old stores load fine (serde defaults); a zero-peer machine behaves exactly as
before, with the sidebar showing one workspace per former project.

## Peering & trust

> **Superseded by SEC-012 (2026-08-08), and the original text is worth keeping in
> mind as a cautionary example.** As designed and shipped, this section exchanged
> each machine's **master token** and then put it in a plaintext `ws://` URL on
> every connection. Three problems in one string: the credential was fleet-level
> authority, it was in a URL (copied into proxy logs, `Referer` headers, shell
> history and crash reports), and it was in the clear. `MESH_VERSION` is now **2**
> and pairs made under version 1 are refused rather than downgraded to.
> Authoritative record: `REMOTE-ACCESS-SECURITY.md`, SEC-012.

- **Pairing UX:** unchanged — Settings → Machines → "Add machine": paste the
  peer's LAN URL + token (the same flow as Hermes/mobile).
- **Two phases, and the master token is never transmitted.** A first fetches
  `GET /api/peer/identity` over plain HTTP: machine id, name, certificate
  authority, mesh port, and a single-use challenge. Nothing there is secret — a
  certificate's job is to be handed out. A then completes
  `POST /api/peer/pair` on B's **TLS mesh listener**, pinned to the CA it just
  received, carrying `HMAC(B's master token, context ‖ challenge ‖ A's machine id
  ‖ fingerprint(B's CA))`. B recomputes it and compares.

  The CA fingerprint in that message is the part that matters. An attacker who
  intercepts the unauthenticated identity fetch and substitutes their own
  certificate receives a proof computed over *their* fingerprint, which the real
  machine rejects — so the trust-on-first-use hole is closed rather than
  accepted. The challenge is single-use and short-lived, so a captured proof
  cannot be replayed at all.
- **What is exchanged** is a pair of freshly minted per-link credentials, one per
  direction, each independently rotatable. Each side stores the plaintext it will
  *present* and only a hash of what it will *accept*. No master token is stored
  and none crosses the wire. A paired machine is trusted to describe its own
  callers, not to be another machine.
- **Transport:** one persistent outbound **`wss`** per online peer to
  `0.0.0.0:<port+2>`, verified against the CA pinned at pairing, with the
  credential in an `Authorization` header. The TLS name is a synthetic
  `<machine-id>.threadknot.mesh` resolved to whichever address hint is being
  tried — so identity is checked while the address stays disposable, which is the
  invariant the DHCP-resilience section below already depends on. A machine that
  takes over an address completes the TCP connection and then fails the
  handshake. Reconnect with capped backoff; presence = socket up + ping.
- **Authority travels with the request.** A routed request carries the
  *originating* caller's grants (`mesh` frame field, or the
  `X-Threadknot-Mesh-Grants` header for splices and the byte proxy), so a phone
  denied `terminal` here cannot obtain one by asking a peer. Under the original
  design every routed request arrived as the peer's owner.

## Discovery & DHCP resilience (three layers)

1. **mDNS/DNS-SD** — advertise `_threadknot._tcp.local.` (TXT: machineId, port,
   meshVersion) via `mdns-sd`; browse continuously. A known machineId seen at
   a new address ⇒ update + reconnect. Also feeds "discovered, not yet
   paired" entries in Settings.
2. **Active announce** — on startup and on interface-set change (~30s poll),
   push `peer.announce {machineId, addresses, port}` to all known peers;
   authenticated announces update `peers.json`. If my IP changed and yours
   didn't, I can still reach you to tell you.
3. **machineId is the only durable key** — every reconnect re-verifies
   `device.info.machineId` against the registry before trusting the address.

Failure floor (both re-DHCP simultaneously AND mDNS blocked): manual address
edit in Settings.

## Sidebar & UI model

- **Sidebar top level = workspaces** (renameable; kebab: rename / manage
  roots / archive-delete semantics unchanged per thread). Under each
  workspace: its threads across all machines, newest-first as today, each
  remote thread wearing a small **machine chip** ("mac-mini"); offline peers'
  threads render greyed from the local header cache with an "offline" chip.
- **New thread in a workspace:** picker = machine (online status + which
  agents it advertises) → that machine's **registered roots only** for this
  workspace (no subfolder browsing, by decision). One root ⇒ preselected.
- **Manage roots:** workspace settings lists members per machine; "attach
  root" picks a machine then uses the DirPicker **proxied to that machine**
  to choose the folder (this is folder selection for *registration*, distinct
  from the roots-only rule at thread creation).
- Solo windows, terminals, git/files panes stay per-project (machine-local
  concepts); a remote thread's workspace panes proxy read ops in Phase 4.

## Thread routing

- `thread.create` gains `{workspaceId, machineId, projectId}` (projectId =
  the chosen root's project on that machine). `machineId == self` ⇒ exactly
  today's code path.
- Remote ⇒ the serving Threadknot proxies creation over the peer socket; the
  owning machine stores the thread + event log ("in that machine forever").
  All subsequent `thread.*` RPCs (turn.start, interrupt, approvals, rename,
  archive, delete, setAgent/setSettings, event fetch) route by the thread's
  `machineId`; events stream back over the peer socket and re-broadcast to
  local UI subscribers. Persistence lives on the owner only.
- Workspace thread listing = local threads + live query of online member
  machines, merged; remote **headers cached** locally for offline rendering.
- Immutability enforced server-side: no RPC changes `machineId`.

## Mobile

Unchanged app: the phone talks to its paired machine, which proxies the mesh —
multi-machine for free. Push for remote-thread events flows owner → your
server → phone via existing `push.rs`; verify explicitly in Phase 4.

---

## Phases

**Phase 0 — Identity + migration (small, ship first) — ✅ DONE**
- `device.json` (machineId := server_id) + `device.info` RPC (adapt Oz
  `mesh/device.rs`, fix hostname; skip all other mesh modules).
- Store migration: thread `machineId` stamping + workspace-per-project
  wrapping (workspace id := project id).
- Additive protocol types (Rust + TS): `Workspace`, `thread.machineId`,
  `workspace.*` RPCs (`list`, `rename`), `state.changed` scope `workspaces`.
- Sidebar switches to rendering workspaces (1:1 with old projects at this
  point, so visually near-identical; rename works and now renames the
  workspace).
- `THREADKNOT_DATA_DIR` env override for `data_dir()` (two instances on one box).
- Gate: old store migrates; zero-peer behavior identical; all tests pass.

**Phase 1 — Peering — ✅ DONE** (`peers.rs` registry, `peernet.rs` runtime,
`peer.add`/`peer.remove`/`peer.list`, `/api/peer/pair`, Settings → machines)
- `peers.json` + `peer.pair`/`peer.unpair` mutual exchange + persistent peer
  sockets + presence; Settings → Machines (add via URL+token, online dots,
  rename, remove).
- Gate: two instances on this box pair mutually; kill one → presence flips;
  headless smoke script proves it.

**Phase 2 — Discovery & DHCP resilience — ✅ DONE** (mDNS advertise/browse in
`peernet.rs`, interface watcher + announce, discovered-not-paired in Settings)
- `mdns-sd` advertise/browse; interface-change watcher + `peer.announce`;
  address-list maintenance keyed by machineId; "discovered, not paired"
  entries in Settings.
- Gate: move a test instance to a new port/address; peer re-resolves without
  manual edits (simulated DHCP move).

**Phase 3 — Workspaces across machines — ✅ DONE** (attachRoot/detachRoot,
mesh.createProject/workspaceUpsert/rewrapProject, resync-on-connect,
manage-roots modal, remote DirPicker via `fs.listDir{machineId}`)
- `workspace.attachRoot {workspaceId, machineId, path}`: creates/uses a
  project on the target machine (remote DirPicker proxy for choosing the
  folder), adds the member, replicates the workspace record (LWW rename by
  `updatedAt`) to all member machines.
- `workspace.detachRoot`; workspace-manage UI (roots listed per machine).
- Gate: attach instance-B root to a workspace created on A; both sides show
  identical workspace membership; rename on B propagates to A.

**Phase 4 — Remote threads (the payoff) — ✅ DONE** (machineId routing on
thread/turn/approval/question/fs RPCs, peernet request API, origin-tagged
event relay, machine chips, machine→root new-thread picker)

**Phase 4.5 — Full remote streaming — ✅ DONE** (everything a direct client
gets, through the mesh): the whole `git.*` family and `term.*` route by
machineId; `/file`, `/attachment` and `/artifact-file` stream bytes from the
owner through the local server (the per-link credential is attached
server-side, in a header, never in the URL); `/term`
sockets splice onto the owner's pty (type here, shell runs there); the
workspace panel renders for remote roots from the member snapshot — Files,
Git, Artifacts and Terminal tabs all work on a remote machine's root. The
driven Browser stays machine-local. Frontend auto-routes: repoId/termId/
artifactId/projectId are resolved to their owning machine from workspace
membership, so panes needed no per-call changes.
- Machine → root picker on new-thread (roots only); full `thread.*` proxy
  layer + event relay; remote header cache; machine chips; files/git read
  proxy for remote threads; push-for-remote verification.
- Gate: from A's UI create a thread on B, run a real claude turn, stream,
  interrupt, approve, rename, archive — then the mirror test from B on A.

**Phase 5 — Hardening & polish (remaining)**
- Durable remote thread-header cache (offline peers' threads render after a
  restart); push notifications for remote-thread events via your own server;
  archive restore onto the owner; remote driven-browser (stretch); update
  `DEVELOPMENT.md`/README.

## Testing strategy

Two full instances on this machine (`THREADKNOT_PORT` + `THREADKNOT_DATA_DIR`), driven
by a headless smoke script (extend the `/tmp/threadknot-smoke.mjs` pattern to two
servers): pair → announce with changed address → attach root → cross-create
thread in a shared workspace → real agent turn → interrupt → archive. The Mac
is the integration test at the end of Phase 4.

## Decisions log

- Workspace is a NEW entity above Project; Project keeps today's meaning
  (one folder on one machine). Migration wraps projects 1:1, reusing ids.
- machineId := existing `server_id` (one identity per machine).
- Thread-creation folder choice = registered roots only, never subfolders
  (Spencer, 2026-07-22). Attaching a NEW root does use a (proxied) folder
  picker — that's registration, not thread creation.
- Paired machines authenticate each other by pinned certificate and per-link
  credential, and a routed request carries its original caller's grants rather
  than the peer owner's (SEC-012). The earlier "fully mutually trusted, tokens
  exchanged both ways" model is gone.

## Open questions (none block Phases 0–2)

1. Remote terminals: in scope eventually? (Stretch, Phase 5.)
2. Cross-network peers (Mac off-LAN): document Tailscale-IP peering as the
   supported answer, or require same-LAN for v1?
3. Should a workspace be creatable EMPTY (name first, attach roots later),
   or only via first root attachment? (Plan assumes empty-creatable — it's
   the natural "New workspace" button.)
