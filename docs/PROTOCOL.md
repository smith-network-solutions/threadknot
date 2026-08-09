# Threadknot client ↔ server protocol (v1)

Primary control endpoint: `GET /ws?token=<token>` — all frames JSON text. Dedicated
binary sockets exist for interactive terminals (`/term`, see below) and the driven
browser (`/browser`).
The same server serves the built web UI at `/` (token required via `?token=` on first
load; the UI stores it in localStorage and appends it to the WS URL). The Tauri
desktop shell loads the bundled UI and obtains `{ port, token }` via the
`server_info` Tauri command; a phone browser uses the LAN URL shown in Settings.

## Listeners and ingress policy

The server binds **three** sockets, and which one a request arrived on is what
decides what that request is allowed to be. The policy is a property of the
socket deliberately: it cannot be a header, because "I came from the relay" is
spoofable from the LAN, and it cannot be the source address, because the
desktop's own webview is also loopback. A separate socket is the only version of
this that a caller cannot select for itself.

| listener | dialled by | authenticates with | refuses |
|---|---|---|---|
| `0.0.0.0:<port>` | LAN browsers, the Tauri webview, paired phones on the same network | master token, device bearer, session cookie — and, here only, `?token=` in the URL | nothing further; this is the compatibility door and the LAN product is unchanged |
| `127.0.0.1:<port+1>` | the connector process on this same machine, and therefore a public relay | a native device bearer, or the opaque cookie from `POST /api/session` | any credential in the query string (`400`, even a valid one), the master credential (`403` however presented), a peer credential (`403`) |
| `0.0.0.0:<port+2>`, **TLS** | paired Threadknot machines, and nothing else | exactly one thing: a per-peer credential in an `Authorization: Bearer` header | a credential in the query string (`400`), any principal that is not a peer (`403`, even though valid), an anonymous request (`401`) |

Route *mounting* differs too, rather than routes merely being guarded: `/mcp`
and `GET /api/peer/identity` are absent from the strict listener's router (a
relay has no business carrying either), and `POST /api/peer/pair` exists **only**
on the mesh listener, so that exchange is always inside TLS.

The strict listener is bound unconditionally even when remote access is off,
because it is loopback-only — binding it exposes nothing, and an always-present
socket is what makes the switch instant instead of a restart; every request on
it answers `503` while remote access is off. The mesh listener is *not* gated on
that switch: the mesh is part of the LAN product. A bind failure on either
hardened listener is logged and dropped rather than fatal — a machine where
something else already holds that port must still work as a solo desktop, and
what it loses (the mesh) is reported in `peer.list`.

## Envelopes

Client → server (requests):

```jsonc
{ "id": 17, "type": "project.create", "payload": { ... } }

// On a connection that authenticated as a PEER only, a frame may also carry a
// `mesh` sibling of `payload` naming whose authority it runs with:
{ "id": 17, "type": "turn.start", "payload": { ... }, "mesh": { "onBehalfOf": ["threads"] } }
```

Server → client:

```jsonc
// Reply to a request (same id):
{ "type": "response", "id": 17, "ok": true, "data": { ... } }
{ "type": "response", "id": 17, "ok": false, "error": "message" }

// Broadcast to every connected client:
{ "type": "event", "threadId": "…", "seq": 42, "ts": "2026-07-27T18:42:11.503Z", "speaker": "…", "event": { ... } }   // agent activity; `speaker` is the producing Participant.id (absent for the user's own messages, server-issued notes, and every thread recorded before Parley)
{ "type": "state.changed", "scope": "projects" | "threads" | "schedules" | "terminals" | "artifacts" | "git" | "peers" | "identity" | "themes", "projectId": "…" }  // refetch hint ("identity" = this machine's own profile changed, e.g. a routed remote edit; refetch hello. "themes" = the custom theme set changed; refetch theme.list)
{ "type": "usage", "usage": ProviderUsage[] }  // subscription usage refreshed (sidebar meter)
{ "type": "hermes.statuses", "revision": 42, "statuses": HermesAgentStatus[] }  // a registered Hermes gateway flipped online<->offline, or was added/removed (full fresh snapshot; `revision` is a per-server-process counter — the client drops any frame/snapshot whose revision is <= the highest it has applied)
```

## Requests

| type | payload | data |
|---|---|---|
| `hello` | `{}` | `{ version, gitHash, buildDate, lanUrl, principal, capabilities, agents: AgentInfo[], serverId, serverName, machineId, friendlyName, avatar?, color?, profileUpdatedAt, meshVersion, dictation: { available, hint? } }` — `lanUrl` carries `?token=` for the master principal only; a device gets the origin, and so does a **peer** (it is administering its own machine, so handing it this machine's token-bearing URL would rebuild the leak it fixed one hop out). `principal` is `"master" \| "device" \| "peer"` and `capabilities` is the grant list this connection actually holds — for a peer, the grants of whoever asked *it* (both advisory; enforced server-side). — `version` is git-derived at build time (`0.1.<commit count>`, build.rs), so it bumps with every commit; `gitHash`/`buildDate` identify the exact build. `avatar`/`color`/`profileUpdatedAt` carry this machine's own profile (the peer merges it last-write-wins by `profileUpdatedAt`) |
| `app.changelog` | `{}` | `{ entries: [{ version, hash, date, subject, body }], notes: [{ version, date, notes[] }] }` — both embedded at compile time by build.rs. `notes` are the client-facing update notes (parsed from CHANGELOG.md) shown by the sidebar version popover; `entries` are the raw git log (newest first, last 60 commits), kept internal |
| `device.info` | `{}` | `{ machineId, friendlyName, avatar?, color?, profileUpdatedAt, hostname, os, arch, version, meshVersion, capabilities[] }` — this machine's mesh identity + live capability scan (`run-claude`, `run-codex`, `run-kimi`, …) |
| `device.rename` | `{ name, machineId? }` | `{ friendlyName }` — the name peers see; master principal only. Routable: with another machine's `machineId` it is forwarded to that machine, which renames itself and gossips the change (peer-to-peer last-write-wins, no central authority) |
| `device.setAppearance` | `{ image?, color?, machineId? }` | `{ avatar, color }` - patches this machine's profile picture (image data URL, max 64 KB) and accent color (short CSS color string); `null` clears a field, an absent key leaves it; propagates to peers beside the friendly name (pairing, hello, announce), which merge it last-write-wins by `profileUpdatedAt`; master principal only. Routable: with another machine's `machineId` it edits that machine's real profile everywhere |
| `workspace.list` | `{}` | `{ workspaces: Workspace[] }` — sidebar top-level containers |
| `workspace.rename` | `{ workspaceId, name }` | `Workspace` — bumps `updatedAt` (the LWW clock for cross-machine reconciliation); replicated to every paired machine |
| `workspace.setFavorite` | `{ workspaceId, favorite }` | `Workspace` — stars or unstars the workspace (`favorite` omitted when false), bumps `updatedAt`, and replicates the record like a rename so the flag syncs mesh-wide |
| `workspace.setHidden` | `{ workspaceId, hidden }` | `Workspace` — stashes the workspace out of the sidebar or brings it back (`hidden` omitted when false), bumps `updatedAt`, and replicates like `setFavorite`. Nothing is deleted or detached: roots, chats and running agents are untouched and the flag is presentation only. Set on the **workspace**, not a project, so it takes every root with it (including roots on peers that are offline) and a project put away on one machine is put away on all of them |
| `workspace.setImage` | `{ workspaceId, image }` | `Workspace` — sets or clears (`null`) the compact sidebar image data URL, bumps `updatedAt`, and replicates it with the workspace |
| `workspace.attachRoot` | `{ workspaceId, machineId, path }` | `{ workspace, project }` — creates/reuses a project at `path` on `machineId` (locally or via the peer's `mesh.createProject`), adds the member (with `name`/`path` display snapshots), replicates the record to every paired machine; master only |
| `workspace.detachRoot` | `{ workspaceId, machineId, projectId }` | `Workspace` — removes the member (founding root and last root are protected) and re-wraps the project into its own workspace on its owner so it stays visible; master only |
| `mesh.createProject` | `{ path, name? }` | `Project` — peer-to-peer: create-or-reuse a project by canonical path WITHOUT its own workspace wrapper |
| `mesh.workspaceUpsert` | `{ workspace }` | `{}` — peer-to-peer catalog push: the FULL workspace list syncs mesh-wide (membership is irrelevant — remote-only workspaces render everywhere and route through their owner); whole-record LWW by `updatedAt`, records older than a local tombstone stay dead; uncovered local projects get re-wrapped |
| `mesh.workspaceDelete` | `{ id, deletedAt }` | `{}` — peer-to-peer delete: tombstones + drops the record unless the local copy was edited after `deletedAt` (the edit wins and revives at the next resync); tombstones are re-pushed alongside the catalog at every peer connect |
| `mesh.rewrapProject` | `{ projectId }` | `{}` — peer-to-peer: re-wrap a just-detached project into its own workspace |
| `peer.list` | `{}` | `{ peers: PeerInfo[], discovered: DiscoveredPeer[] }` — paired machines (with live `online` and `needsUpgrade`) + unpaired Threadknots seen via mDNS. Credentials, credential hashes and pinned CAs are never serialized. `needsUpgrade` is reported separately from `online` because a pre-mesh-v2 pair is not offline — it is refused, and only re-pairing fixes it; calling it offline sends someone hunting a network fault that does not exist |
| `peer.add` | `{ url, token? }` | `PeerInfo` — one-paste mutual pairing, in the two phases described below: `GET /api/peer/identity` for the peer's public identity and a single-use challenge, then `POST /api/peer/pair` over TLS pinned to that identity's CA, carrying an HMAC **proof** of the pasted token rather than the token itself. Each side leaves holding a freshly minted per-link credential for the other; no master token is stored or transmitted. Owner only |
| `peer.remove` | `{ machineId }` | `{}` — also purges workspaces living entirely on that machine (no tombstone: re-pairing brings them back); master principal only |
| `peer.setAppearance` | `{ machineId, image?, color? }` | `PeerInfo` - a LOCAL display override for the peer's avatar/accent color (never sent to the peer; wins over the peer-advertised `avatar`/`color` in the UI); same patch semantics as `device.setAppearance`; master principal only |
| `peer.announce` | `{ machineId, addresses[], port }` | `{}` — a peer refreshing its address hints (sent over peer sockets on startup/interface change); only known machine ids are honored |
| `connector.status` | `{}` | `ConnectorStatus` — this machine's hosted-relay connector: `state` (`off` \| `unenrolled` \| `connecting` \| `online` \| `error`), server-assigned `hostname`/`publicOrigin`, `lastError`, `connectedSince`, byte counters, `acceptingNewSessions` + `holdReason`, `trialDaysLeft`, `approval`, `liveStreams`. `trialDaysLeft` exists because `holdReason` is only populated once a hold is already in force — a build watching only that told people their trial had ended on the day it stopped working, which is the one day the information is useless. The whole `connector.*` family is owner-only and deliberately **not** routable: each machine enrolls itself with its own key, and enrolling a peer from here would mean holding a key on behalf of the one machine that should ever hold it |
| `connector.beginApproval` | `{ machineName? }` | `ConnectorApproval` — **the normal way to connect a machine.** Opens a device-approval request: this machine generates its keypair, signs over the key *and* the name, and gets back a URL for the owner to open plus a short `userCode` as a fallback for a box with no browser. Nothing sensitive is returned — the `deviceCode` that collects the enrollment stays in the Rust process, so the panel is safe on a shared screen. The connector then polls on its own, so closing Settings does not abandon an approval mid-grant, and a granted request lands as a normal enrollment (config written, pairing origin provisioned, supervisor kicked). Owner only |
| `connector.cancelApproval` | `{}` | `ConnectorStatus` — stop watching. The request stays valid server-side until it expires; this only stops *this* machine collecting it |
| `connector.enroll` | `{ enrollmentToken, machineName? }` | `ConnectorStatus` — the token path, kept for scripted and headless installs and for anyone already holding a token. Registers this installation with the relay's control plane, signing the console-issued token with this machine's own Ed25519 key so seeing the token is not enough to enroll a different key. The hostname comes back server-assigned and is never proposed here; enrollment is therefore also what provisions the remote pairing origin. Owner only |
| `connector.setEnabled` | `{ enabled }` | `ConnectorStatus` — turning it off drops the tunnel, signs every remote browser session out and closes every socket opened through the strict ingress; the LAN is untouched. Owner only |
| `project.create` | `{ path, name? }` | `Project` |
| `project.list` | `{}` | `{ projects: Project[] }` |
| `project.delete` | `{ projectId }` | `{}` (does NOT touch the folder on disk) |
| `thread.create` | `{ projectId, agent, settings }` | `Thread` |
| `thread.list` | `{ projectId }` | `{ threads: Thread[] }` |
| `thread.get` | `{ threadId }` | `{ thread, events: PersistedEvent[] }` (full replay) |
| `thread.search` | `{ query, threadIds[] }` | `{ threadIds[] }` — case-insensitive full-content search over the requested threads' persisted, user-visible transcript text (messages, reasoning, tools, questions, diffs, statuses, errors, and artifacts). Routes by optional `machineId`, allowing the client to search each owning machine without downloading transcripts. |
| `thread.preview` | `{ threadId }` | `ThreadPreview` — a hover-card summary scanned from the persisted log: last assistant reply (`summary`, ~300 chars), last user message (`lastUser`, ~200 chars), and `turnCount` (count of `turn_completed` events). An unknown or empty thread answers `{ threadId, turnCount: 0 }` with no text. Routes by `machineId` like the rest of `thread.*`, so remote-thread previews resolve on the owner |
| `thread.rename` | `{ threadId, title }` | `Thread` |
| `thread.setFavorite` | `{ threadId, favorite, machineId? }` | `Thread` — stars or unstars the thread (`favorite` omitted when false); broadcasts the updated thread like `thread.rename` and routes by `machineId` to the owning peer |
| `thread.delete` | `{ threadId }` | `{}` |
| `thread.setSettings` | `{ threadId, settings }` | `Thread` |
| `thread.setAgent` | `{ threadId, agent, settings }` | `Thread` — mid-thread provider switch; next `turn.start` routes to the new agent with the conversation handed off (see below). Also re-points the primary builder lane; reviewer lanes are untouched |
| `thread.review` | `{ threadId, agent, model?, effort?, access?, instructions? }` | `Participant` — Parley: adds (or reuses) an adversarial reviewer lane and immediately runs exactly one turn on it. Errors if the thread is busy or has no history. `model` is required for the profile-backed kinds (`hermes`, `claudex`); others fall back to a per-agent default. `access` omitted seats the reviewer with FULL control (no permission prompts); `read`/`edits` is the deliberate restriction that makes it ask, and the brief adapts to match. Lanes are keyed by the full setup (agent+model+effort+access), so a changed setup seats a new lane. The reviewer reads the thread through the ordinary handoff seed, and yields the floor back to the builder at its turn boundary, so the user's next `turn.start` reaches the builder. Its brief lands in the log as a `user_message` with `injected: true` |
| `thread.parley.start` | `{ threadId, reviewers: [{ agent, model?, effort?, access?, name?, personality? }], rounds?, execute?, instructions? }` | `Thread` — Parley debate: seats 1–4 reviewer lanes and runs structured rounds (each reviewer attacks, the builder answers objectors, repeat) until every reviewer concedes — then, with `execute: true`, one builder turn implements the conceded fixes — or `rounds` (default 2, clamped 1..=6) is hit and the leftovers escalate. `name` sets the lane's display name and `personality` is folded into every brief (personas are just named, reusable reviewer specs). Errors if busy, empty, or a parley is already running. `turn.start` mid-parley queues the message onto `parley.pendingUser` instead of failing. See the Parley section for the state machine |
| `persona.save` | `{ persona }` | `ReviewerPersona` — create/replace a named reviewer preset (matched by `id`, empty mints one). Any authenticated client (a paired phone shapes its reviewers too); pulses `identity` (personas ride `hello`) |
| `persona.delete` | `{ personaId }` | `{}` |
| `turn.start` | `{ threadId, text, attachments? }` | `{}` (events stream separately) — `attachments` is `OutgoingAttachment[]` (`{ name, mimeType, data }`, base64 without a `data:` prefix); bytes persist under `~/.threadknot/attachments/<threadId>/` and the persisted `user_message` event carries `AttachmentMeta[]` (`{ id, name, mimeType, sizeBytes }`), served token-gated via `GET /attachment`. All drivers deliver image attachments (claude: base64 content block; codex: `input` image data URL; kimi: ACP image block; hermes: `image_url` data-URL part on `/v1/runs`) |
| `turn.steer` | `{ threadId, text }` | `{}` — deliver extra context while a turn is RUNNING without interrupting it. Claude/Claudex write the note to the open stream-json stdin; Codex calls native `turn/steer`; Kimi ACP has no steer request, so Threadknot queues the note and promotes it to a new `session/prompt` at the current provider-turn boundary. If the Threadknot turn ended in the meantime the hub degrades to a normal `turn.start`. During Claude approval waits the frame queues on stdin and is consumed once approval resolves. `machineId`-routable like the rest of `turn.*` |
| `turn.interrupt` | `{ threadId }` | `{}` |
| `approval.respond` | `{ threadId, approvalId, optionId }` | `{}` |
| `question.respond` | `{ threadId, requestId, answers }` | `{}` — `answers` is `{ [questionId]: string[] }` (one element for single-select; a free-text "Other" answer is just a string in the array) |
| `hermes.agent.list` | `{}` | `{ agents: HermesAgentInfo[] }` — registered remote Hermes gateways (`{ id, name, baseUrl, model, createdAt }`; the API key is write-only) |
| `hermes.agent.add` | `{ baseUrl, apiKey, name? }` | `HermesAgentInfo` — probes `/health` + `/v1/models` before storing; name defaults to the advertised model (profile) name; master principal only |
| `hermes.agent.remove` | `{ agentId }` | `{}` — master principal only |
| `hermes.agent.setImage` | `{ agentId, image }` | `HermesAgentInfo` — sets or clears (`null`) the agent's sidebar profile picture; master principal only |
| `hermes.agent.setAvatar` | `{ agentId, image }` | `HermesAgentInfo` - the same picture as `setImage` under its current wire name (the record serializes it as both `image` and `avatar`), with the tighter 64 KB avatar bound; master principal only |
| `hermes.agent.details` | `{ agentId }` | `{ health: {ok, version}, skills: [{name, description, category}], toolsets: [{name, label, description, enabled, tools[]}] }` — live from the gateway (`/v1/skills`, `/v1/toolsets`; MCP servers mount as toolsets) |
| `hermes.agent.statuses` | `{}` | `{ revision, statuses: HermesAgentStatus[] }` — live Online/Offline presence for every registered gateway (initial snapshot on connect). A background poller (spawned only after the port bind succeeds, and aborted when the server stops) re-probes each gateway's `/health` every 20s (5s per-probe timeout, all gateways probed concurrently); it broadcasts a `hermes.statuses` frame whenever a gateway flips online<->offline or is added/removed, and re-probes immediately on add/remove. Both the snapshot and the broadcast carry a monotonically increasing per-server-process `revision` so the client can drop whichever of the two loses their delivery race (it keeps the highest revision seen, resetting to none on every reconnect). `HermesAgentStatus = { agentId, online, lastCheckedAt, sinceAt, latencyMs?, version? }` — `latencyMs`/`version` present only when online; `lastCheckedAt` is the most recent probe time (truthful but not a live-freshness signal, since it bumps every poll while broadcasts are change-only), `sinceAt` is when the current online/offline state was entered (preserved across non-flipping polls, driving "offline since …"). The commit intersects results against the current registry, so a gateway removed mid-probe is dropped, not resurrected. Statuses exist only for gateways registered on THIS machine |
| `theme.list` | `{}` | `{ themes: CustomTheme[] }` — the machine's user-crafted appearance themes (machine-local, stored server-side in `themes.json`; not mesh-replicated) |
| `theme.save` | `{ theme: CustomTheme }` | `CustomTheme` — upsert by `id` (an unknown id creates); the server sets `updatedAt` and, on a new record, `createdAt`. Bounds enforced: `name` trimmed to 1..64 chars, at most 40 themes, `backgroundImage` at most 2 MB as a string, `backgroundDim` clamped to 0..0.9, `backgroundZoom` clamped to 1..3 (non-finite → 1), `backgroundX`/`backgroundY` clamped to -100..100 (non-finite → 0; a placement pan as a percent of the available overflow), `colors` at most 24 entries of at most 32 chars each. All `background*` fields are optional and stay absent when unset. Broadcasts `state.changed { scope: "themes" }` |
| `theme.remove` | `{ themeId }` | `{}` — deletes the theme (unknown id errors); broadcasts `state.changed { scope: "themes" }` |
| `theme.aiPalette` | `{ imageDataUrl, hint?, machineId? }` | `AiPalette` — `{ family: "dark" \| "light", accent, colors: Record<slot, "#rrggbb">, name? }`. Shells out to the local Claude CLI to design a chat-app palette that complements the wallpaper (`imageDataUrl`, a downscaled `data:image/png\|jpeg;base64,…` URL, at most 3 MB; `hint` free text at most 200 chars). Routable: `machineId` names a machine that has the CLI. The 10 `colors` slots are keyed like `CustomTheme.colors` (bg, bg-raise, panel, panel-2, panel-3, line, line-2, text, dim, faint); every value is validated server-side as a 6-digit lowercase hex and `family` must be dark or light, else a readable error |
| `claudex.profile.list` | `{}` | `{ profiles: ClaudexProfileInfo[] }` — registered Claudex profiles; `authToken` and `sensitive` env values are write-only (`hasAuthToken` reports presence, sensitive env values come back `null`) |
| `claudex.profile.add` | `ClaudexProfileInput` | `ClaudexProfileInfo` — requires `name`, `baseUrl`, `model`; a `sidecar` is refused unless `baseUrl` is loopback; master principal only |
| `claudex.profile.update` | `ClaudexProfileInput & { profileId }` | `ClaudexProfileInfo` — omitted fields keep their stored value, including `authToken` (send `""` to clear); stops any sidecar this profile had started; master principal only |
| `claudex.profile.remove` | `{ profileId }` | `{}` — also stops its sidecar; master principal only |
| `claudex.profile.setAvatar` | `{ profileId, image }` | `ClaudexProfileInfo` — sets or clears (`null`) the profile's picker/sidebar image; master principal only |
| `claudex.profile.test` | `{ profileId }` | `{ sidecar: "external" \| "managed", baseUrl }` — reachability check that **starts** a configured sidecar; errors if nothing answers and none is configured; master principal only |
| `claudex.profile.status` | `{ profileId }` | `{ sidecar: "external" \| "managed" \| "stopped" }` — read-only; starts nothing |
| `schedule.create` | `{ projectId, agent, settings, name?, prompt, cadence }` | `Schedule` — name defaults to a prompt prefix; created enabled with `nextRunAt` planned |
| `schedule.list` | `{}` | `{ schedules: Schedule[] }` |
| `schedule.update` | `{ scheduleId, name?, prompt?, cadence?, enabled?, agent?, settings?, projectId? }` | `Schedule` — any edit re-plans `nextRunAt` |
| `schedule.delete` | `{ scheduleId }` | `{}` |
| `schedule.run` | `{ scheduleId }` | `{ threadId }` — fire immediately (does not move the plan) |
| `usage.get` | `{}` | `{ usage: ProviderUsage[] }` — cached snapshots (kicks a fetch if the cache is cold; results arrive via the `usage` broadcast) |
| `usage.refresh` | `{}` | `{}` — force a re-fetch, bypassing the freshness floor; snapshot arrives via the `usage` broadcast |
| `dictation.start` | `{}` | `{ recordingId }` — records the mic of the machine serving this socket; never peer routed, master token only |
| `dictation.stop` | `{ recordingId }` | `{ text }` — stops and transcribes with the selected local/API provider; `""` means the clip held no speech |
| `dictation.cancel` | `{ recordingId }` | `{}` — throw the clip away without transcribing it |
| `dictation.settings.get` | `{}` | Secret-free voice settings (`provider`, API base/model, `hasApiKey`, local/capture readiness); master only |
| `dictation.settings.save` | `{ provider, baseUrl, model, apiKey? }` | Saves local or OpenAI-compatible API transcription settings; the write-only key is never returned; master only |
| `fs.listDir` | `{ path? }` | `{ path, parent, entries: [{name, path, isDir}] }` (dirs only; for the phone's folder picker; `path` omitted → home dir) |
| `term.list` | `{ projectId }` | `{ terms: TermInfo[] }` — persisted tabs for the project, each with a live `alive` flag |
| `term.create` | `{ projectId, name? }` | `TermInfo` — a new tab (name defaults to `Terminal N`); the pty spawns lazily on first `/term` attach |
| `term.rename` | `{ termId, name }` | `TermInfo` |
| `term.delete` | `{ termId }` | `{}` — permanent: kills any live shell and deletes the saved scrollback |
| `artifacts.list` | `{ projectId, threadId? }` | `{ artifacts: ArtifactRecord[] }` — deliverables the agent produced (newest first); project-scoped, or one thread with `threadId` |
| `artifacts.delete` | `{ artifactId }` | `{}` — removes the artifact index entry and Threadknot's durable snapshot; does not delete the original project file |
| `git.repos` | `{ projectId }` | `{ repos: GitRepoInfo[] }` — scans the project folder for repos, reconciles persisted records, returns live summaries |
| `git.status` | `{ repoId }` | `GitStatus` — branch/upstream/ahead-behind + per-file entries |
| `git.diff` | `{ repoId, path, scope }` | `{ path, unified, truncated, binary }` — `scope`: `"staged"` \| `"worktree"` \| `"untracked"`; capped at 256 KB |
| `git.stage` | `{ repoId, paths }` | `GitStatus` (fresh, post-mutation) |
| `git.unstage` | `{ repoId, paths }` | `GitStatus` |
| `git.discard` | `{ repoId, paths }` | `GitStatus` — DESTRUCTIVE: restores tracked files, deletes untracked ones (UI confirms first) |
| `git.commit` | `{ repoId, message }` | `GitStatus & { hash, subject }` — commits what's staged |
| `git.branches` | `{ repoId }` | `{ current, detached, branches, remoteBranches }` — local heads plus remote-only branch names (remote prefix stripped); checking out a remote-only name DWIMs a tracking branch |
| `git.checkout` | `{ repoId, branch, create? }` | `GitStatus` — `create: true` = `checkout -b` |
| `git.push` | `{ repoId }` | `GitStatus & { output }` — plain `git push`; auto-retries `-u origin HEAD` when the branch has no upstream |
| `git.pull` | `{ repoId }` | `GitStatus & { output }` — `git pull --ff-only` |
| `git.commitMany` | `{ projectId, entries: [{repoId, message, stageAll?}], link? }` | `{ results: GitOpResult[], changeId? }` — one action, several repos: optional `add -A` then commit each; `link` + ≥2 entries stamps every message with a shared `Threadknot-Change: <changeId>` trailer; per-repo failures land in `results`, not errors |
| `git.checkoutMany` | `{ projectId, repoIds, branch }` | `{ results: GitOpResult[] }` — same branch across repos: switches where it exists, creates (`-b`) where it doesn't (`created` per repo) |
| `git.pr` | `{ repoId, title?, body? }` | `{ url?, output }` — `gh pr create` (`--fill` without a title) using the installed gh CLI's auth |

Where a row above says **"master principal only"** it means the *owner* test, not
the narrow one: this machine's master credential, or a peer link carrying that
peer's own owner (the fleet view exists so that sitting at one machine can
administer another, and that was already true when peers authenticated as
Master). A peer acting for one of *its* paired devices does not pass. The narrow
"exactly this machine's master credential" test is used in one place only —
withholding the token-bearing `lanUrl` in `hello` — because that answer would
hand over the credential itself.

Any terminal mutation broadcasts `state.changed { scope: "terminals", projectId }`. A new
or updated artifact, or an artifact deletion, broadcasts
`state.changed { scope: "artifacts", projectId }`. Every git mutation broadcasts
`state.changed { scope: "git", projectId }` so other clients' fleet views refresh.

### Mesh (multi-machine)

Full design + phases: `docs/MULTI-MACHINE.md`. Summary of the wire pieces:

- **Identity**: `machineId` == `server.json`'s `server_id`. It is the ONLY
  durable key — addresses/ports are hints. `meshVersion` (int, currently **2**)
  is exchanged at pairing; mismatches refuse with "update Threadknot on the
  older machine".
- **Workspace**: `{ id, name, createdAt, updatedAt, members: [{machineId,
  projectId}] }` — the renameable sidebar container above (unchanged)
  `Project`. Migration wraps each project 1:1 reusing the project id.
  `Thread` carries a required, immutable `machineId`.
- **Pairing**: two phases (`GET /api/peer/identity`, then `POST /api/peer/pair`
  on the mesh listener inside TLS). Each side ends up holding a per-link
  credential the other minted, plus the other's pinned certificate authority, in
  `~/.threadknot/peers.json` (0600). No master token is stored or transmitted.
  Full field list below.
- **Presence/transport**: each side keeps a persistent outbound **`wss://`** to
  every paired peer's mesh listener, verified against the CA pinned at pairing
  and authenticated by a per-peer credential in an `Authorization` header — never
  a URL, which is copied into proxy logs, shell history and crash reports. The
  URI names the synthetic `<machineId>.threadknot.mesh` host so the certificate
  check asks "is this really machine X" while the address stays disposable;
  `hello.machineId` is still verified against the registry before an address is
  trusted. `state.changed { scope: "peers" }` broadcasts on any
  presence/registry change.
- **DHCP resilience**: mDNS `_threadknot._tcp.local.` advertise+browse (TXT:
  machineId, name, meshVersion), plus `peer.announce` pushed over live peer
  sockets on startup and whenever the local interface set changes.
- **Remote routing**: `thread.*`, `turn.*`, `approval.respond`,
  `question.respond`, `thread.archive`, `fs.listDir`/`fs.tree`/`fs.read`,
  the whole `git.*` family, `term.*`, `artifacts.list`/`artifacts.delete` and
  `browser.profile.*` accept an optional `machineId`. When it names another machine the server
  forwards the request verbatim (sans `machineId`) over that peer's socket
  and returns its response — the OWNER executes and persists; clients supply
  the id from the thread/workspace they're acting on. Local calls omit it and
  are byte-identical to pre-mesh. The forwarded frame carries a `mesh`
  assertion describing the original caller's authority, so the owner enforces
  the same grants the near side did (see below).
- **Byte + socket streaming**: `/file`, `/attachment` and `/artifact-file`
  accept `machineId` and stream the bytes from the owner through this server
  over the mesh listener (the peer credential is attached server-side in a
  header — client tokens never work on a peer directly, and every
  credential-bearing query key is stripped before forwarding).
  `/term?machineId=…` splices the WebSocket onto
  the owner's pty endpoint, so remote terminals type/echo live, and
  `/browser?machineId=…` does the same onto the owner's Chrome, so a chat on a
  peer shows and drives that machine's browser (and its stored logins) from
  here. `/term` needs the `terminal` + `mesh` grants; the `/browser` splice is
  owner-only, deliberately stricter than the `browser` + `mesh` that would now
  suffice (see the driven-browser section). These paths are
  connection-scoped rather than framed, so they carry the caller's authority in
  the `X-Threadknot-Mesh-Grants` header instead of a `mesh` frame field.
- **Event relay**: each peer socket relays the peer's own `event` and
  `state.changed` broadcasts to local clients with an added
  `origin: <machineId>` field. Frames that already carry `origin` are never
  relayed again (loop prevention); locally produced frames never have it.

### Peer pairing (two phases)

Pairing bootstraps trust between two machines that have never met, over a
network that may have an attacker on it. It is split in two so that nothing
secret ever crosses in the clear, and so that the second phase can be
authenticated by *proof* rather than by transmission.

**Phase 1 — `GET /api/peer/identity`** on the target's plain-HTTP LAN listener.
Unauthenticated on purpose: everything it returns is public. It lives on the
plain listener because it *is* the bootstrap — the caller has no certificate to
verify against yet, which is precisely what this hands them.

```jsonc
{ "machineId": "…", "name": "…", "avatar": null, "color": null, "profileUpdatedAt": null,
  "port": 42800, "meshPort": 42802, "meshCa": "-----BEGIN CERTIFICATE-----…",
  "meshVersion": 2, "addresses": ["192.168.0.10", …],
  "challenge": "…" }   // single-use, 120 s, in memory only
```

**Phase 2 — `POST /api/peer/pair`**, on the target's **mesh listener**, over TLS
pinned to the `meshCa` phase 1 returned. Mounted only there, and authenticated
by the proof rather than by a peer credential — a machine being paired for the
first time does not have one yet.

```jsonc
// initiator → target
{ "machineId": "…", "name": "…", "avatar": null, "color": null, "profileUpdatedAt": null,
  "port": 42800, "meshPort": 42802, "meshCa": "…", "meshVersion": 2,
  "addresses": [ … ], "challenge": "…",
  "proof": "…",                 // HMAC-SHA256, see below
  "credentialForYou": "…" }     // minted by the initiator; the target will present this to it

// target → initiator (its own identity, plus the other half of the exchange)
{ "machineId": "…", "name": "…", …, "meshCa": "…", "meshVersion": 2,
  "credentialForYou": "…" }     // minted by the target; the initiator will present this to it
```

`proof` is `HMAC-SHA256(key = the target's master token, "threadknot-peer-pair-v2"
‖ challenge ‖ 0x00 ‖ initiator machineId ‖ 0x00 ‖ hex(SHA-256(meshCa)))`. Three
properties matter, and each is why one input is in there:

- The master token is the **key**, so it proves knowledge without being sent. The
  exchange this replaced put each machine's master token in a request body and
  then stored it forever as the peer credential.
- The message binds the **fingerprint of the CA the initiator actually saw**. Phase
  1 is unauthenticated, so an attacker can intercept it and substitute their own
  certificate — but then the proof is computed over *their* fingerprint, the real
  machine recomputes with its own, and the comparison fails. That is what closes
  the trust-on-first-use hole instead of accepting it.
- It binds the initiator's `machineId`, so a captured proof cannot be replayed to
  pair a different machine. The challenge is single-use and short-lived, so it
  cannot be replayed at all — and it is checked *before* the proof, so there is
  nothing to grind against.

Both sides then hold, per pair: the credential they present outbound, the hash
of the credential they accept inbound, the peer's pinned CA, and the peer's mesh
port. Each direction is independently rotatable, and re-pairing the same machine
is the supported way to rotate. `mint_credential` is 32 bytes of OS randomness,
base64url unpadded; inbound credentials are stored as SHA-256 and compared in
constant time, scanned across every pair rather than looked up by a claimed
machine id (a caller that could name which pair to check could grind one at a
time).

**Version 1 pairs are refused, never downgraded.** `MESH_VERSION` is `2`, and a
pair is unusable if its `meshVersion` is lower or if any of the pinned CA,
outbound credential or mesh port is missing. Such a pair is reported as
`needsUpgrade` in `peer.list` and every use of it errors with "update Threadknot
on that machine, then pair the two again". A silent fallback to the old
plaintext transport would mean the fix only applied to pairs made after the
upgrade, which is the same as not shipping it — an attacker on the LAN would
simply wait for the one legacy pair.

### Mesh principal propagation (the `mesh` frame field)

A peer credential says **which machine** a request came from. It does not say
**whose authority** it carries, and it must not: one peer socket multiplexes
requests from every client on that machine — its owner, and each of its paired
phones — so the authority has to be per request.

```jsonc
{ "id": 17, "type": "term.create", "payload": { … },
  "mesh": { "onBehalfOf": ["threads", "files"] } }
```

- It is honoured **only** on a connection that authenticated with a peer
  credential. A phone or a LAN browser can put it in a frame all it likes; it is
  discarded. That discard is the security property.
- `mesh` is a **sibling of `payload`, not a field inside it**, so it can never
  collide with a real request parameter and cannot be smuggled in by a client
  that controls a payload.
- **Absent** `onBehalfOf` means "that machine's own owner", which carries
  machine-administration authority here — the fleet view exists so that sitting
  at one machine can administer another. It does not confer this machine's own
  master credential: `hello` still withholds the token-bearing `lanUrl` from a
  peer, because handing it over would rebuild the leak it was closing, one hop
  out. Machine-to-machine plumbing (`mesh.workspaceUpsert` and friends) sends no
  assertion, because there is no human behind it.
- A **present** `onBehalfOf` list can only **narrow**. There is no assertion a
  peer can make that grants more than its own owner already had, and a request
  forwarded through a third machine carries the *original* caller's authority
  unchanged rather than being re-widened at each hop.
- An **unrecognised capability name is dropped, not honoured and not an error**. A
  newer peer must never be able to widen an older machine by naming a capability
  it does not understand, and erroring would let a newer peer break an older one
  by merely mentioning one.
- Resolving the credential alone gives a peer **no grants at all**. The authority
  comes from the frame (or, on the paths below, the header) and nowhere else:
  defaulting to "the peer's owner" at credential resolution would mean any handler
  that forgot to consult it silently ran as Master again, which is the bug being
  fixed.

For the connection-scoped paths there is no frame to put this in, so the same
assertion travels in the **`X-Threadknot-Mesh-Grants`** header: a comma-separated
capability list on the splices (`/term`, `/browser`) and the byte proxy (`/file`,
`/attachment`, `/artifact-file`), absent meaning the peer's own owner. It is read
**only** on the mesh listener — on any other door "absent" describes every
request ever made, so reading it there would silently promote ordinary LAN
requests to owner authority. The long-lived peer `/ws` socket deliberately sends
no such header; its authority is per frame.

### Git (multi-repo projects)

A project is a FOLDER, not a repo: it may contain several git repositories
(`frontend/`, `backend/`, `mobile/`, …). `git.repos` discovers them by scanning
for `.git` entries (depth ≤ 4, skipping `.git`/`node_modules`/`target`; a repo's
subtree is not descended into, so submodules fold into their parent; a project
root that is itself a repo yields exactly one repo at `relPath ""` — the
mono-repo case is just N=1). Discovered repos persist as records keyed by
`projectId` + `relPath`, so `repoId` is stable across restarts; records whose
folder disappears are pruned on the next scan. There is deliberately no
"project-wide git status" — every operation is repo-scoped and the client
aggregates.

All operations shell out to the installed `git` CLI (`GIT_TERMINAL_PROMPT=0`, 30 s
timeout, 120 s for push/pull/fetch), so push/pull use the user's real SSH keys and
credential helpers. Status is parsed from `git status --porcelain=v2 --branch -z`.
A `.git` entry is validated before a folder counts as a repo (a `.git` dir must
contain HEAD; a `.git` pointer file must resolve) — corrupt/empty `.git` folders
are treated as plain directories. Linked worktrees and submodules (`.git` files
pointing into `.git/worktrees/` or `.git/modules/`) are checkouts of a repo
listed elsewhere and are excluded, and hidden directories (`.worktrees/`, caches)
are never descended into.

Multi-repo turns surface in chat too: diff cards carry a repo badge (client-side
longest-relPath-prefix match; repo summaries lazy-load when a thread opens).
Commits made through `git.commitMany` with `link` can be correlated later with
`git log --grep "Threadknot-Change: <id>"` across the repos.

`GitOpResult = { repoId, ok, hash?, subject?, created?, error? }`.

```ts
interface GitRepoInfo {           // git.repos summaries (fleet overview)
  id: string; projectId: string; relPath: string; name: string;
  branch?: string; detached?: boolean; upstream?: string | null;
  ahead?: number; behind?: number;
  staged?: number; unstaged?: number; untracked?: number; conflicted?: number;
  lastCommit?: { hash: string; subject: string; at: string };
  error?: string;                 // set → `git status` failed; live fields absent
}

interface GitStatus {
  repoId: string; branch: string; detached: boolean; upstream?: string | null;
  ahead: number; behind: number;
  staged: number; unstaged: number; untracked: number; conflicted: number;
  entries: {                      // one per changed file
    path: string; origPath?: string;   // origPath on renames
    x: string; y: string;              // porcelain staged/worktree letters ("." = clean side)
    kind: "changed" | "untracked" | "conflicted";
  }[];
}
```

### Artifacts (agent-produced deliverables)

Files the agent produces *for the user* (a generated PDF, exported CSV, report,
archive, …) are surfaced as **artifacts**, through two channels:

1. **Published (primary).** Threadknot's MCP server exposes a `publish_artifact`
   tool (`{ path, title?, description? }`) to every driven agent alongside the
   `browser_*` tools. The agent explicitly registers a deliverable right after
   creating it; the file is snapshot + indexed immediately (mid-turn chat card,
   `origin: "published"`), and is exempt from turn-end detection. This is the
   authoritative signal — the tool description instructs agents to publish
   every deliverable and never source edits, scratch files, or user inputs.
2. **Detected (conservative fallback).** At the turn boundary, deliverable-typed
   files are diffed against a turn-start baseline (`.threadknot/`, VCS/dep/build
   dirs excluded — user attachments are materialized under `.threadknot/attachments`
   and must never surface as artifacts). Only *newly created* files can be
   automatic: standalone document/archive formats (PDF, Office, zip, …) count on
   their own; ambiguous project formats (`.md`, `.html`, `.json`, images, CSV)
   need the exact path named by the user's request or the agent's final prose.
   A *modified* file survives only when the final prose names it and it is
   either high-confidence or already one of the thread's artifacts (a refreshed
   deliverable). More than eight candidates is treated as build fan-out: only
   newly created files named in the final prose are retained, capped at eight.

Each artifact is snapshot-copied to
`~/.threadknot/artifacts/<threadId>/<id>.<ext>` so it stays viewable/downloadable
even if the working-tree file later moves or is deleted. An `artifact` agent
event is emitted (chat card); the durable bytes are fetched from:

```
GET /artifact-file?id=<artifactId>&token=<token>[&download=1]   // raw snapshot bytes; download=1 adds Content-Disposition: attachment
```

`ArtifactRecord = { id, threadId, projectId, name, relPath, mimeType, sizeBytes, source, op ("created"|"modified"), origin ("published"|"detected"), description?, createdAt }`.

## Domain objects

```ts
type Agent = "claude" | "codex" | "kimi" | "hermes" | "claudex";
// "kimi" = the local Kimi Code CLI over ACP, authenticated by `kimi login`
// and charged to the user's Kimi subscription rather tha Threadknot API credits.
// "hermes" = remote Hermes Agent gateways (Nous Research hermes-agent). One
// agent KIND for all registered gateways; the specific agent rides in
// ThreadSettings.model as the hermes.json registry entry's id.
// "claudex" = the Claude Code harness driven by a NON-Anthropic model over an
// Anthropic-compatible bridge. Same driver as "claude", different process
// environment. Like hermes it is one agent KIND for all registered profiles,
// with the profile id riding in ThreadSettings.model (claudex.json).

interface Project { id: string; name: string; path: string; createdAt: string }
// Workspace carries optional `image?: string`, a compact image data URL that
// is replicated mesh-wide with the workspace record.
// Reserved id "hermes-home": the hidden folder-less project hosting Hermes
// threads. Created lazily by `thread.create`, never wrapped in a workspace;
// the sidebar renders its threads in the dedicated Hermes section.

interface ClaudexProfileInfo {
  id: string; name: string; avatar?: string | null;
  baseUrl: string;         // Anthropic-compatible endpoint, e.g. http://127.0.0.1:18765
  model: string;           // model id as the GATEWAY names it, e.g. gpt-5.6-sol
  smallModel?: string | null;   // Claude Code's cheap background calls + titles
  contextWindow?: number | null;// real upstream window (no [1m] hint applies)
  efforts: string[]; defaultEffort?: string | null;
  hasAuthToken: boolean;   // the token itself is never returned
  env: { name: string; value: string | null; sensitive: boolean }[];
  sidecar?: { command: string; args: string[] } | null;
  createdAt: string;
}

interface ThreadSettings {
  model: string;          // provider model id (see AgentInfo.models)
                          // claudex: the ClaudexProfileInfo id
                          // hermes:  the HermesAgentInfo id
  // Claude: absent means "Default (<AgentModel.defaultEffort>)" in the UI and
  // deliberately omits the CLI's --effort flag. A concrete value is an override.
  effort?: "low" | "medium" | "high" | "max";   // reasoning effort override
  wideContext?: boolean;  // claude only: 1M context window
  claudeChrome?: boolean; // native claude only: launch the CLI with --chrome
  access: "read" | "edits" | "full";  // how much the agent may do without asking
  mode: "plan" | "build";
}

interface Thread {
  id: string; projectId: string;
  agent: Agent;                // agent the NEXT turn runs on (mutable: thread.setAgent)
  title: string;
  settings: ThreadSettings;
  providerSessionId?: string;  // legacy mirror of the current agent's session id
  // Per-PARTICIPANT native session + how far into the event log it has absorbed
  // (coveredUntilSeq), keyed by Participant.id. Lets a lane resume its OWN
  // session when switched back to, seeded only with the delta it missed.
  // `profile` is set for agent kinds with more than one backend (claudex): a
  // claude session id only exists inside the CLAUDE_CONFIG_DIR that made it,
  // so an anchor from another profile reads as absent and is re-seeded.
  // Records written before Parley key these by AGENT NAME, which is exactly the
  // implicit builder's id — so they keep resolving with no migration.
  sessionAnchors?: {
    [participantId: string]: { sessionId: string; coveredUntilSeq: number; profile?: string };
  };
  // Present only while a remote Hermes Runs API job is active. Persisted so a
  // restarted Threadknot reconnects to that same run instead of duplicating it.
  providerRunId?: string;
  status: "idle" | "running" | "waiting_approval";
  // Parley lanes. ABSENT/EMPTY is the common case and means one implicit
  // builder derived from `agent` + `settings`; materialized when a reviewer
  // joins. Clients must synthesize the implicit lane rather than assume this
  // is populated (see threadParticipants() in src/lib/protocol.ts).
  participants?: Participant[];
  // Lane currently mid-turn, or the one that spoke most recently.
  activeSpeaker?: string;
  // The debate currently running on this thread (thread.parley.start), if any.
  parley?: ParleyState;
  createdAt: string; updatedAt: string;
}

// One lane in a thread: a provider session with its own identity, model and
// access, anchored into the thread's single shared event log. Two lanes MAY run
// the same agent and model — they are told apart by `id`, which is what
// `PersistedEvent.speaker` carries and what keys `sessionAnchors`.
interface Participant {
  id: string;            // implicit builder: the agent name ("claude"); else a uuid
  agent: Agent;
  settings: ThreadSettings;   // reviewers: mode:"build" always, access:"read" unless granted more
  role: "builder" | "reviewer";
  name: string;          // "Codex (reviewer)"
  color: string;         // lane color used by attributed views
}

// A live debate on a thread (thread.parley.start). The hub's scheduler is a
// deterministic state machine driven by this struct — at each turn boundary
// it scores the finished turn (`inFlight`) and seats the next speaker.
interface ParleyState {
  lanes: string[];       // reviewer Participant.ids, in speaking order
  round: number;         // 1-based round being spoken
  maxRounds: number;     // clamped 1..=6
  next: number;          // index into lanes; == lanes.length once all reviewers spoke
  objectors: string[];   // lanes that ended OBJECTING this round
  hadObjections: boolean; // any objection ever — gates the execution turn
  execute: boolean;      // run the builder's implement-what-you-conceded turn on convergence
  pendingUser?: string;  // user message queued mid-parley; speaks next
  inFlight?: "reviewer" | "answer" | "execute" | "user";
}

interface AgentInfo {
  id: Agent; name: string;            // "Claude Code", "Codex", "Kimi Code", …
  available: boolean; authHint?: string;  // e.g. "run `claude login`" when not authed
  models: { id: string; name: string; supportsWideContext?: boolean;
    fixedContextWindow?: number }[];
  defaultModel: string;
}
// Hermes entries in AgentInfo.models and HermesAgentInfo may carry
// `image?: string`, the agent profile picture shown in the sidebar.
```

On the first user turn, an ordinary `New thread` gets an immediate truncated
fallback title, then Threadknot asynchronously asks the selected provider's
lightweight subscription-backed model for a concise title (Claude Haiku 4.5 or
GPT-5.6 Luna at medium reasoning). The generation process is ephemeral and
separate from the chat's native provider session. Its result is applied only if
the title still equals the automatic fallback, so a manual rename or a
pre-titled scheduled run is never overwritten. Failures leave the fallback in
place and do not affect the turn.

### Scheduled runs

```ts
// Human presets, no cron (modeled on Codex's ScheduledTaskSchedule).
// Times are LOCAL "HH:MM"; days use 0=Sunday..6=Saturday.
type Cadence =
  | { type: "hourly"; everyHours: number }   // fires at :00 of hours divisible by everyHours
  | { type: "daily"; time: string }
  | { type: "weekdays"; time: string }
  | { type: "weekly"; days: number[]; time: string };

interface Schedule {
  id: string; projectId: string;
  agent: Agent; settings: ThreadSettings;
  name: string;            // used as the thread title ("<name> · Jul 21, 09:00")
  prompt: string;          // the turn text each firing sends
  cadence: Cadence;
  enabled: boolean;
  createdAt: string;
  lastRunAt?: string; lastThreadId?: string;
  lastError?: string;      // fire failure, or a "missed while not running" note
  nextRunAt?: string;      // planned next firing (absent for degenerate cadences)
}
```

Semantics: the server runs a 30 s scheduler loop. Each firing creates a **fresh
thread** in the project and starts a normal turn with `prompt`, so events,
persistence, and client notifications behave exactly like a hand-started turn.
A due time missed by ≤ 60 min (suspend, brief restart) still fires; older
misses are skipped with an explanatory `lastError` and the schedule rolls
forward. Any schedule mutation broadcasts `state.changed { scope: "schedules" }`.

### Access × mode mapping

| access/mode | Claude Code | Codex | Kimi Code |
|---|---|---|---|
| `plan` mode | `--permission-mode plan` | sandbox `read-only`, approvalPolicy `never`, planning prompt | ACP mode `plan`, plan review surfaced |
| `build` + `read` | `--permission-mode default` (every mutation asks) | sandbox `read-only`, approvalPolicy `on-request` | ACP mode `default`, mutations ask |
| `build` + `edits` | `--permission-mode acceptEdits` | sandbox `workspace-write`, approvalPolicy `on-request` | ACP mode `default`, edit/delete/move auto-approved |
| `build` + `full` | `--permission-mode bypassPermissions` | sandbox `danger-full-access`, approvalPolicy `never` | ACP mode `yolo` |

Hermes is exempt from this table: remote gateways govern their own tool access
and approvals server-side (their `approvals` config), so the composer hides the
access/mode controls for hermes threads. Approval gates the gateway raises
arrive as normal `approval_request` events with options `once` / `session` /
`always` / `deny`.

## Agent events (`event.event`)

Normalized across providers. `seq` is a per-thread monotonically increasing integer;
events with `transient: true` semantics (deltas) are NOT persisted and carry `seq: -1`.

```ts
type AgentEvent =
  | { kind: "user_message"; text: string; mid_turn?: boolean; injected?: boolean }
  |   // mid_turn: note submitted while work is active (turn.steer); the
  |   // provider may inject it immediately or queue it for its next boundary
  |   // injected: machine-issued brief (a Parley role prompt), not typed by the
  |   //   human. Still a real turn boundary; the UI renders it as a collapsed
  |   //   divider and the handoff seed labels it as a brief, not as the user.
  | { kind: "turn_started"; agent?: Agent; model?: string }  // provenance (absent in old logs)
  | { kind: "assistant_delta"; text: string }          // transient
  | { kind: "assistant_message"; text: string }        // final, markdown
  | { kind: "thinking_delta"; text: string }           // transient
  | { kind: "thinking"; text: string }
  | { kind: "tool_start"; callId: string; name: string; detail: string }
  |   // detail: command line, file path, pattern, etc. (already humanized)
  | { kind: "tool_output_delta"; callId: string; text: string }  // transient
  | { kind: "tool_end"; callId: string; name: string; output?: string; isError?: boolean }
  | { kind: "file_diff"; path: string; unified: string }
  | { kind: "artifact"; id: string; name: string; relPath: string; mimeType: string; sizeBytes: number; op: "created" | "modified" }  // a produced deliverable (see Artifacts)
  | { kind: "approval_request"; approvalId: string; approvalKind: "tool" | "exec" | "patch" | "plan";
      title: string; detail: string;                    // detail: command / diff / plan markdown
      options: { id: string; label: string; tone: "allow" | "deny" | "allowAlways" }[] }
  | { kind: "approval_resolved"; approvalId: string; optionId: string }
  | { kind: "question_request"; requestId: string;
      questions: { id: string; header: string; question: string;
        options: { label: string; description: string }[];
        multiSelect: boolean; allowOther: boolean; isSecret: boolean }[] }
  |   // agent asked clarifying questions (Claude AskUserQuestion / Codex requestUserInput).
      // Answer via question.respond { answers: { [question.id]: string[] } }.
  | { kind: "question_resolved"; requestId: string; answers?: Record<string, string[]> }
  | { kind: "context_usage"; usage: { usedTokens?: number; maxTokens?: number;
      contextPct?: number } }                              // live/replayable meter snapshot
  | { kind: "turn_completed"; usage?: { inputTokens?: number; outputTokens?: number;
      usedTokens?: number; maxTokens?: number; contextPct?: number; costUsd?: number } }
  | { kind: "turn_aborted" }
  | { kind: "session_started"; providerSessionId: string; model: string; agent?: Agent }
  | { kind: "status"; text: string }                    // e.g. "compacting context"
  | { kind: "subagent_started"; taskId: string; toolUseId: string;
      description: string; subagentType: string; background: boolean; prompt?: string }
  | { kind: "subagent_progress"; taskId: string; activity: string; text: string } // transient
  | { kind: "subagent_completed"; taskId: string; status: string; summary?: string }
  | { kind: "error"; message: string };

interface PersistedEvent { seq: number; ts: string; event: AgentEvent }
```

## Mid-thread agent switching (context handoff)

Modeled on Traycer's design (traycerai/traycer): the event log is the canonical,
provider-agnostic conversation; provider sessions are disposable views of it.

- `thread.setAgent` only flips `thread.agent` + `settings` (thread must be idle).
- On the next `turn.start`, the hub sees the live driver's agent ≠ `thread.agent`
  and routes to a fresh driver for the new provider:
  - it resumes that provider's OWN previous session if a `sessionAnchor` exists
    (`claude --resume` / codex `thread/resume` / Kimi ACP `session/resume`),
  - and renders a **handoff seed** — a deterministic text transcript of every
    event past that anchor's `coveredUntilSeq` (all events for a first visit) —
    prepended to the first user message (a bare seed message would itself
    trigger a turn).
- `coveredUntilSeq` advances to the latest seq on each `turn_completed` /
  `turn_aborted`, so re-switching only ever seeds the missed delta.
- After a Threadknot restart, a saved Codex id is explicitly reactivated with
  `thread/resume` before the next `turn/start`. If native resume fails, a new
  provider thread receives the full persisted Threadknot transcript as its first
  turn's seed rather than leaving the chat unusable.
- Startup automatically continues Claude/Codex/Kimi turns that were `running` when
  Threadknot stopped. Approval/question waits remain paused for the user. Hermes
  runs are reattached by their persisted `providerRunId` because the remote
  gateway job itself survives the local restart. Recovery begins only after the
  server owns its listening port, preventing duplicate work from a second launch.
- Claude has a bounded pre-response recovery path. If a turn produces no provider
  model or tool output for 90 seconds, Threadknot emits status events, retires the
  CLI process, resumes the saved Claude session on a fresh process, and re-sends
  the same logical request once. Local CLI init/status/context frames do not
  count as progress. Once provider output has begun, automatic replay is disabled
  to avoid duplicating tool mutations; if the one recovery attempt also stalls,
  the turn ends with an explicit error.
- `turn.interrupt` is a Claude process boundary as well as a turn boundary.
  Threadknot closes the old driver's command channel before `turn_aborted`, gives
  the CLI a short grace period to record interruption, and then retires it. A
  later `turn.start` therefore creates a clean process and cannot be consumed by
  a late result from the interrupted turn.
- Fidelity caveat: the seed is narrative text (messages, tool runs, trimmed
  outputs/diffs, approval outcomes, question answers). It is NOT a replayable
  native tool-call trace, and switching restarts provider-side prompt caching
  for the seeded tokens.

## Parley: multi-agent lanes and debates (`thread.review`, `thread.parley.start`)

Adversarial review reuses the handoff machinery above rather than adding a
parallel mechanism — the reviewer is just another lane reading the same log.
Design and roadmap: **`docs/PARLEY.md`**.

- A thread holds `participants` (lanes). Absent/empty means one implicit builder
  derived from `agent` + `settings`; clients synthesize it (`threadParticipants`)
  instead of assuming the field is populated.
- `sessionAnchors` is keyed by `Participant.id`, NOT agent kind. That is what
  lets two lanes run the same agent and model independently — keying by agent
  would hand the second lane the first one's live session and context. The
  implicit builder's id is the agent's wire name, so pre-Parley anchors resolve
  unchanged.
- The live driver is keyed by `(participantId, claudexProfile)`, so routing to a
  different lane spawns that lane's own process and seeds it from its own anchor.
- Reviewers default to `access: "full"` — the same hands-off control the
  builder runs with — because a reviewer that must ask permission for every
  command stalls the whole debate on a human click. `read`/`edits` is the
  deliberate opt-in restriction that makes it ask (the dialog says so), and
  the brief adapts so it never forbids editing a lane that can write. Turns
  are strictly sequential, so two full-access lanes still never touch the
  working tree at the same time. A lane is keyed by its full setup — agent +
  model + effort + access — so re-reviewing with changed permissions seats a
  NEW lane rather than silently reusing the old one's session under the wrong
  settings.
- **Personas.** Named, reusable reviewer presets — a display name, the
  agent/model/effort/access that powers it, and a personality folded into
  every brief — live in `personas.json` beside the other registries, ride
  `hello.personas`, and are edited via `persona.save`/`persona.delete`
  (Master only). Three built-ins (The Skeptic / Second Opinion / The
  Contrarian) seed on first run so a first debate is one click. The persona's
  name becomes the lane name, and its id is the lane's identity
  (`Participant.persona`): two personas on the IDENTICAL setup seat two
  separate lanes with separate sessions, while re-running one persona —
  renamed or not — reuses its lane. `parley.personalities` carries each
  lane's voice so every round's brief keeps it.
- A reviewer never keeps the floor: at its `turn_completed` / `turn_aborted` the
  hub restores `agent`/`settings`/`activeSpeaker` to the primary builder, so the
  composer always addresses the lane doing the work. Re-reviewing reuses the same
  lane, so its session survives and it is seeded only with what changed. A fatal
  driver `error` restores the floor the same way (without advancing any anchor).
- **Debates.** `thread.parley.start` seats 1–4 reviewer lanes and stores a
  `parley` state on the Thread (`{ lanes, round, maxRounds, next, objectors,
  hadObjections, execute, pendingUser?, inFlight? }`). The scheduler is a
  deterministic state machine in the hub (`parley_decide`), never an LLM: at
  each turn boundary it scores the finished turn and seats the next speaker —
  each reviewer in order, then the builder to answer the round's objectors,
  then the next round. Reviewer briefs demand a final `VERDICT: CONCEDED |
  OBJECTING` line, which is what the scheduler parses (fallback: the "no
  material objection" phrase; missing both counts as objecting).
- A debate never just stops: when its rounds end it runs ONE closing builder
  turn that leaves a deliverable. Converged with `execute` → the conceded
  fixes are implemented. Converged without → the builder writes THE PLAN
  (numbered, concrete, who conceded what and why). Round cap → the builder
  writes OPEN QUESTIONS: what's settled, each unresolved objection with both
  positions, and the exact one-sentence questions the user must answer
  (`parley.wrap`: `execute` / `plan` / `escalation`, flight `verdict`).
  `turn.interrupt` still stops the debate at its boundary.
- **Interjection.** `turn.start` while a parley holds the floor does NOT fail
  the idle check: the text queues on `parley.pendingUser`, the debate pauses
  at the next turn boundary, the user's message runs as a normal builder turn,
  and the debate (or its closing summary) resumes where it stopped.
  `turn.steer` still reaches the current speaker mid-turn (provider permitting).
- Turn-taking is strictly sequential even in a debate — `status` stays a
  truthful whole-room aggregate because only one lane can hold the floor.

## Provider usage (sidebar meter)

```ts
interface RateWindow {
  label: string;         // "5h" | "Week" | "3d" | …
  usedPercent: number;
  resetsAt?: string;     // ISO
  windowMins?: number;
}
interface ProviderUsage {
  agent: Agent;
  available: boolean;
  plan?: string;         // "Max" | "Pro" | …
  windows?: RateWindow[];
  error?: string;        // when unavailable
  fetchedAt: string;
}
```

Server-side sources (same ones Traycer uses):
- **claude** — GET `https://api.anthropic.com/api/oauth/usage` with the OAuth
  token from `~/.claude/.credentials.json` (the endpoint behind the CLI's
  `/usage`). The normalized `limits` list supplies the session, all-model weekly,
  and model-scoped weekly windows (currently Fable); legacy top-level fields are
  retained as a fallback. Can 429 with a multi-minute penalty, hence the
  conservative cadence.
- **codex** — short-lived `codex app-server` probe → `account/rateLimits/read`;
  live sessions additionally feed `account/rateLimits/updated` notifications
  into the cache for free mid-turn freshness.

Cadence: full poll every 15 min; kicked (with a 120 s floor + 3 s debounce) on
every `turn_completed` and by `usage.get` on a cold cache; `usage.refresh`
bypasses the floor. Failed fetches keep the last good snapshot.

## Driven browser (`/browser` socket + `/mcp` tools)

Every local thread has one isolated, in-memory Chrome session shared by its agent
and all attached Threadknot clients. The browser survives socket disconnects and
workspace-tab changes; the last frame, current URL, open dialog, and recent agent
activity are replayed when a client reattaches. An explicit `reset` is the only UI
control that replaces it. A dead Chrome process is replaced automatically on the
next attach or agent action.

`GET /browser?token=…&session=<threadId>` attaches the human control surface.

- **server → client binary**: a live JPEG screencast frame.
- **server → client text**:
  - `{ "type": "nav", "url": "…" }`
  - `{ "type": "activityHistory", "activities": [BrowserActivity, …] }`
  - `{ "type": "activity", "activity": BrowserActivity }`
  - `{ "type": "dialog", "dialog": BrowserDialog | null }`
  - `{ "type": "tabs", "tabs": [{ "id": "…", "url": "…", "title": "…", "active": true }, …] }`
  - `{ "type": "engine", "state": "stopped" }` immediately before the socket closes and reconnects to a replacement engine
- **client → server text**:
  - navigation: `{ "type": "navigate", "url": "…" }`, `back`, `forward`, `reload`
  - viewport: `{ "type": "resize", "width": 390, "height": 844 }`
  - pointer: `{ "type": "mouse", "event": "moved"|"pressed"|"released", "x": 10, "y": 20, "button": "left"|"right"|"middle", "clickCount": 1 }`
  - wheel: `{ "type": "wheel", "x": 10, "y": 20, "deltaX": 0, "deltaY": 400 }`
  - keyboard: `{ "type": "key", "event": "down"|"up", "key": "a", "code": "KeyA", "keyCode": 65, "text": "a", "modifiers": 0 }`. CDP modifier bits are Alt=1, Control=2, Meta=4, Shift=8.
  - dialogs: `{ "type": "dialog", "accept": true, "promptText": "optional" }`
  - tabs: `{ "type": "newTab", "url": "optional" }`, `{ "type": "switchTab", "id": "…" }`, `{ "type": "closeTab", "id": "optional" }`
  - lifecycle: `{ "type": "reset" }`

`BrowserActivity` is `{ id, actor, phase, action, label, detail?, x?, y?,
bounds?, ts }`, where phase is `started | targeting | completed | failed` and
bounds is `{ x, y, width, height }` in native viewport coordinates. Updates with
the same id replace the prior state while retaining its last target. Typed and
filled values are deliberately replaced with a privacy label in this human-facing
feed. `BrowserDialog` is `{ kind, message, defaultPrompt?, url }`.

The agent connects to `POST /mcp` using the thread-scoped bearer token minted when
its provider driver starts. Besides `publish_artifact`, `tools/list` advertises
the `browser_*` family: navigation/history/reload/resize, tab
list/new/switch/close, semantic snapshot, screenshot, click, hover, drag,
fill/select/check/fill-form, type/key chords, scroll, evaluate, condition waits,
dialog handling, file upload, console, network, network body, downloads, and
status. A popup or `target=_blank` page becomes the shared active tab
automatically, while the previous tab remains available to both human and agent.
`status`, `console`, `network`, `tabs` and `downloads` answer without starting a
browser when none is running.

`browser_snapshot` returns a nested accessibility outline followed by a PNG,
covering the main frame **and every child frame** in one ref space (Chrome runs
with site isolation off inside this profile so one CDP session can reach
cross-origin frames). Indentation is meaningful: repeated controls are told
apart by the row, list item or dialog containing them. Each interactive node
receives a document-scoped ref such as `e12`; ordinary actions should use those
refs, with CSS selectors (retried inside child frames) or viewport coordinates
as fallbacks. Navigation, reload, same-URL navigation and viewport changes all
invalidate the ref table, and raw CDP node failures are rewritten into the same
"take a fresh snapshot" instruction rather than surfacing as protocol strings.
Most mutating calls accept `includeSnapshot: true` to combine action and
observation. Agent actions are serialized and given a bounded settle window so
concurrent tool calls cannot create an incoherent human-visible sequence.

Key events carry `text` (`Enter` → `"\r"`, `Tab` → `"\t"`), which is what makes
Chrome perform the key's default action; `browser_type` dispatches one real
keydown/keyup per character unless `fast: true` is passed. `browser_screenshot`
writes a PNG to disk (viewport, `fullPage`, or a single element) and returns its
path for `publish_artifact`. Downloads are captured with
`Browser.setDownloadBehavior` into the session directory and listed by
`browser_downloads`.

A thread's browser is disposable unless its settings name a signed-in profile
(`ThreadSettings.browserProfileId`). Profiles are managed with
`browser.profile.list` / `.create` / `.update` / `.delete` — all but `list`
require an owner principal (creating a profile, widening the sites it may reach,
or erasing one is an authority change over stored logins; a revocable phone
credential may drive a session the owner already set up but not decide what it
can reach), and all four are routable, so `machineId` manages
another machine's logins from here. `GET /browser` takes `machineId` too and
splices the socket onto that machine's Chrome (like `/term`), which is how a
remote machine's browser gets signed in without sitting at it; splicing
requires an **owner** principal — which a peer acting for its own owner satisfies,
since the fleet view exists so that sitting at one machine can administer
another. It stays owner-only even though the splice now carries the caller's own
grants and the far side enforces them (so `browser` + `mesh` would in fact be
sound): relaxing it widens authority over stored logins, which is a product
decision rather than a consequence of the transport change. Profiles live in
`~/.threadknot/browser-profiles.json` with
their Chrome data under `~/.threadknot/browser/profiles/<id>`. A signed-in session
may only load documents from the profile's `origins` (enforced by Fetch
interception in the browser, not just at the tool boundary; `origins: ["*"]` —
what an empty sites list normalizes to — allows any http/https site while still
excluding non-web schemes like `file://`), refuses
`browser_evaluate`, keeps Chrome's site isolation, is limited to one thread at a
time, and on `GET /browser` requires the separate `signedBrowser` grant — which
is never part of the default set, on any pairing, because such a session can act
as the logged-in account. `browser_status`
reports `signedInProfile` and `allowedSites`.

Disposable Chromes use a unique temporary profile rather than the user's real
browser profile; profiles orphaned by a kill or crash are swept at startup, and a
session with no viewer and no agent activity for 30 minutes is closed. Sessions
are closed with `Browser.close` so Chrome flushes its cookie jar. File upload is the
only host-to-page byte path and accepts only canonical existing files inside the
thread's project root; screenshot destinations are held to the same root unless
the default session directory is used.

## Terminals (`/term` socket)

Terminals are project-bound pty shells. The **record** (`TermInfo { id, name, alive,
createdAt }`) is persisted via the `term.*` requests above, so tabs survive an app
restart and can be renamed. The **live shell** streams over a dedicated socket:

`GET /term?token=…&project=<projectId>&term=<termId>&cols=<n>&rows=<n>` — attaches to
(or lazily spawns) the pty for that terminal. The `term` must be a known record for the
project, else the upgrade is refused (`404`, no orphan sessions).

- **server → client**: binary frames = raw pty output; text `{ "type": "exit", "code": n }` on shell exit, `{ "type": "replay", "bytes": n }` immediately before the scrollback frame, `{ "type": "role", "responder": bool }` on every responder election.
- **client → server** (JSON text): `{ "type": "input", "data": "…" }`, `{ "type": "resize", "cols": n, "rows": n }`, `{ "type": "kill" }`, `{ "type": "claim" }`.

**Query replies (`replay` / `role` / `claim`).** A terminal emulator *answers* the
queries it parses — cursor position (`ESC[6n` → `ESC[…R`), device attributes (`ESC[c`),
OSC 4/10/11/12 colour — and each answer goes into the pty as if typed. One pty here has
N attached emulators and replays its history to each, so both must be gated or the
answers surface at the shell prompt as `;1R10;rgb:d8d8/dddd/e9e911…`:

- `replay` announces that the **next binary frame is history, not live output**. The
  client mutes everything xterm emits until it has finished parsing that frame,
  otherwise every attach (reload, phone wake, app restart) re-answers questions the
  original programs asked and consumed long ago — the answers accumulate forever.
- exactly one client is the **responder**: the newest attach, re-elected when one
  detaches, and claimable with `claim` (a visible view takes the role, since a frozen
  background tab keeps its socket but stops answering). Non-responders swallow the query
  sequences themselves, so live queries are answered once no matter how many views
  are open. Clients that ignore `role` keep answering — fine while only one is attached.

`kill` ends the shell process but keeps the tab: the client gets an `exit` frame and can
re-attach (press Enter) to respawn a fresh shell in the same cwd. Permanent removal is
`term.delete`. The shell is `$SHELL -l` (Unix) / PowerShell (Windows), cwd = project path.

**Scrollback + cwd restore.** A session's output ring (≤512 KiB) is mirrored to
`~/.threadknot/terminals/<termId>.scrollback`, and the shell's working directory to
`<termId>.cwd`, on a ~2s timer while it runs. A pty process can't survive an app restart,
so on the next attach the saved bytes are replayed into the terminal (with a `previous
session restored` marker) and a fresh shell is spawned **in the saved cwd** — the user
sees prior history and reopens where they were. cwd tracking reads `/proc/<pid>/cwd`
(Linux); on macOS/Windows it falls back to the project root. The shell's own command
history is preserved by the shell itself.

## Notifications

The owning server attaches optional `notice: {title, body}` copy to live,
persisted `turn_completed`, `error`, `approval_request`, and `question_request`
event frames. Completion bodies use the current turn's final assistant message,
with deterministic file/artifact/task fallbacks; the other event kinds use their
actual question, approval detail, or error. The field is optional so older peers
remain compatible, and clients retain generic copy as a fallback.

The frontend suppresses the alert when the window is focused AND that thread is
open (Traycer's presence rule). Being focused on a different Threadknot chat does
not suppress the OS notification. The same server-composed copy feeds Expo push,
so desktop/browser and sleeping-phone alerts agree. Surfaces: native desktop notification via the Tauri
`notify` command (notify-rust with `desktop-entry=threadknot` on Linux and the
installed `com.smithnetwork.threadknot` AppUserModelID on Windows), Web Notification API where it exists (needs
HTTPS — not the LAN phone URL), and always an in-app toast + WebAudio chime
(+ vibration on phones). Toggles persist in localStorage
(`threadknot.notifyOff`, `threadknot.soundOff`,
`threadknot.notifyPreviewsOff`). Disabling previews keeps generic status copy and
syncs that privacy choice to the paired-phone record when running in the mobile
shell. Settings has a native **send test**
button; `threadknot --test-notification` exercises the same backend without opening
a window. Windows native notifications require the NSIS-installed build (the
portable CI executable has no registered toast identity).

## Persistence (server side)

`~/.threadknot/`
- `server.json` — `{ port, token, server_id }` (token generated once; `server_id`
  is the stable install identity mobile push routing keys on — auto-migrated in)
- `mobile.json` — paired mobile devices (credential *hashes* only, Expo push
  tokens, notification prefs)
- `peers.json` (0600) — paired machines: per-pair outbound credential, inbound
  credential *hash*, the peer's pinned CA, its mesh port. None of it is
  serialized to clients
- `mesh-ca.pem` / `mesh-ca.key` / `mesh-leaf.pem` / `mesh-leaf.key` — this
  machine's mesh identity. The certificates are public; the two keys are `0600`.
  **Deleting the CA silently unpairs every peer**, because each one pinned it at
  pairing
- `connector.json` + `connector.key` (0600) — the hosted-relay connector's
  server-assigned installation id/hostname, and its Ed25519 identity
- `hermes.json` — registered remote Hermes gateways (base URL + bearer API key
  in plaintext — needed for outbound calls; never serialized to clients)
- `projects.json` — project + thread + schedule + terminal + artifact index
- `threads/<threadId>.jsonl` — one `PersistedEvent` per line, append-only
- `artifacts/<threadId>/<id>.<ext>` — durable snapshots of produced deliverables
- `terminals/<termId>.scrollback` — raw pty output ring (≤512 KiB), replayed on reopen
- `terminals/<termId>.cwd` — shell's last-known working directory, restored on reopen (Linux)

## Reconnection

Clients reconnect with exponential backoff and re-issue `thread.get` for the open
thread; `seq` lets the client drop duplicates (ignore events with `seq <=` the last
seen persisted seq; deltas always apply).

## Mobile companion (summary)

The Expo app in `mobile/` authenticates with revocable per-device credentials
(`amd_…`) minted by `POST /api/mobile/pair`. That call takes **either** the
master token **or** a one-time `pairingCode` scanned off a QR — the desktop
mints one with `mobile.pair.begin` and drops it with `mobile.pair.cancel`
(both master-only, like the device admin requests). The code is single-use,
expires in 180s, lives only in memory, and is what the QR encodes, so a screen
showing a pairing QR never leaks this machine's master token. Device
credentials are **capability-scoped**, not equivalent to the master token: each
device stores a grant set (`threads`, `files`, `git`, `terminal`, `browser`,
`signedBrowser`, `mesh`) chosen by the owner and bound server-side to the
pairing code, so the joining client cannot widen it. Grants are checked
centrally in `handle_request` (and on `/ws`, `/term`, `/browser`, `/file`,
`/attachment`, `/artifact-file`) **before** any `machineId` routing. The grants
now also *travel* with a routed request (the `mesh` frame field, or
`X-Threadknot-Mesh-Grants` on the byte and splice paths) so the far side enforces
them for itself — that closed the residual gap where only the originating side
knew who was really asking. The near-side check stays anyway, so a denial is
reported by the machine the person is actually talking to.
`mobile.device.list` / `mobile.device.revoke` / `mobile.device.setCapabilities`
/ `mobile.pair.*` stay master-only (surfaced in desktop Settings); revoking a
device or narrowing its grants closes the sockets it already holds.

### The strict remote ingress

One of the three listeners above: `127.0.0.1:<port+1>`, dialled only by the
connector process on this machine, which is what a relay's traffic arrives
through.

On the strict ingress: credential-bearing query keys are `400` even when valid;
the master credential is `403` however presented; a peer credential is `403`
too, which keeps a compromised relay out of the mesh's trust path entirely;
`/mcp` and `/api/peer/identity` are not mounted; and authentication is either a
native device bearer token or an
opaque cookie from `POST /api/session` (one-time pairing code or device bearer
in, host-scoped `HttpOnly; Secure; SameSite=Strict` cookie plus a double-submit
CSRF token out; `DELETE` signs out). Cookie-authenticated state changes must
send `X-Threadknot-Csrf`. Responses carry HSTS, a CSP with `frame-ancestors
'none'`, `Referrer-Policy: no-referrer`, and no CORS headers. Remote access is
off by default (`remote.get` / `remote.set` and `connector.*`, owner-only);
turning it off drops
every browser session and every socket opened through that ingress, and leaves
the LAN untouched. `mobile.pair.begin` takes `target: "lan" | "remote"`, and the
remote address comes from stored configuration — never from the request `Host`,
which would be a pairing-redirection hole.

`hello` additionally returns `serverId`, `serverName`, `principal`
(`"master" | "device" | "peer"`) and `capabilities`, and gives the token-bearing
`lanUrl` to the local master principal only — a device, and a peer, receive the
origin alone. See
`docs/REMOTE-ACCESS-SECURITY.md`. The Rust server pushes `turn_completed`,
`approval_request`, `question_request` (and opt-in `error`) through the Expo
Push API with data `{version, serverId, projectId, threadId, eventKind}`.
Full details: `docs/MOBILE.md`.
