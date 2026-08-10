# Dispatch — handing work to another agent, on another machine

> Status: **PHASES 0–3 BUILT, PHASE 4 PARTLY (2026-08-09)**. Read alongside
> `MULTI-MACHINE.md` (the mesh this rides on), `PARLEY.md` (the multi-agent
> primitive this is deliberately *not*), and `PROTOCOL.md`.
>
> **This file is the design record — why it is built this way, and what is not
> built. For how to actually drive it, read `USING-DISPATCH.md`.** Keep usage
> there and rationale here: two copies of either is the pair that drifts.
>
> What is proven, and how: `scripts/dispatch-smoke.py` pairs two headless
> instances and asserts local exec, routed exec, unattended long jobs, routed
> cancel, cross-machine dispatch creation, report delivery, and a report raised
> while the parent was **offline** arriving on reconnect.
> `scripts/dispatch-live.py` runs the real thing — a Claude-configured parent
> dispatches to **Codex** and to **Kimi**, each does the work on disk, files a
> structured `report_result`, and the parent's feed shows the worker row and its
> completion. `scripts/schedule-dispatch-smoke.py` covers the Phase 4 half: a
> schedule carrying a dispatch block, fired unattended, fanning out to both
> machines, reporting the target it could not reach instead of skipping it, and
> settling its coordinator when the last worker reports. Cross-OS runs against
> the real **Windows 11** box and the **Mac** passed with real Codex workers.
> All pass.
>
> Phase 4, what landed: **dispatch from a `Schedule`** (`ScheduleDispatch`, the
> coordinator-thread shape, fan-out, honest partial-failure reporting) and a
> **push notification when a worker reports** (`PushKind::DispatchFinished`).
>
> Not yet done, and not pretended otherwise:
> - **Reusable dispatch templates as their own registry.** A *disabled*
>   dispatching schedule plus "Run now" is already a saved, one-click,
>   fully-specified dispatch, which is most of what a template is for; a
>   `personas.rs`-shaped registry would only add a second place to define one.
>   Revisit if that turns out to be too oblique to find.
> - **Dispatch from a Hermes gateway agent, and dispatch as a Parley step.**
> - **Worktree isolation** (`isolate: "worktree"`). Concurrent dispatches into
>   one checkout are **serialized** instead — correct, but slower than the plan's
>   intent. The queue-and-release path is implemented and is what runs.
> - **Sidebar nesting of child threads** and a dedicated **crew card**. The
>   pinned agent panel is the crew view today: one row per worker with a machine
>   chip, an agent chip, live activity and a click through to its thread.
> - **No way to start a one-off dispatch from the UI.** Agent-issued (the MCP
>   tool) and schedule-issued are the two doors; there is no button.
> - **`syncRef` has never been exercised against a live remote.** It is
>   implemented and reachable from the schedule form; nothing has run it.
>
> *Dispatch*: to send off to a destination with a purpose. Name is cheap to
> change — `crew`, `detail`, `delegate` were the alternatives.

## The two asks, and why they are one feature

1. **Cross-harness delegation.** Start a thread on Claude Opus, tell it to plan
   and drive a feature, and have it hand the actual implementation to Codex or
   Kimi and read back the result.
2. **Cross-machine execution.** Start a thread on the Arch box, ask for a
   production build of Threadknot, and have it produce Linux, Windows and macOS
   builds by running on the paired Windows machine and the paired Mac.

These are the same operation with different values in one field. A dispatch is:

```
(brief, agent, model, machine, root, access, isolation) → result
```

Ask 1 varies `agent`. Ask 2 varies `machine`. Building them as two features
produces two orchestrators, two ledgers, two result contracts and two places
where a worker can be left running with nobody watching. Building the tuple once
produces both, and the interesting combination — *"have Codex do the Windows
build while Kimi does the Mac"* — falls out without being designed for.

## Why this is mostly already built

| Existing piece | What Dispatch needs it for |
| --- | --- |
| `Hub::sessions: HashMap<threadId, SessionHandle>` (`agents/mod.rs:744`) | Threads already run concurrently. N workers is N threads, not new concurrency machinery |
| Remote threads: `thread.create`/`turn.start` routed by `machineId` (`server.rs:2164` `ROUTABLE`) | Starting a worker on the Mac is starting a thread on the Mac — shipped in Phase 4 |
| Event relay, origin-tagged (`peernet.rs:573`) | The worker's stream already reaches this machine's clients |
| Byte proxy `/artifact-file?machineId=` (Phase 4.5) | The Windows `.msi` is openable from the Arch thread without copying it anywhere |
| `git.push` / `git.pull` are routable (`git.rs:664`) | The only supported code channel to the other machines, already drivable from here |
| Per-thread MCP endpoint (`mcp.rs`) | Where the driving agent gets `dispatch` as a callable tool |
| `SubagentStarted/Progress/Completed` + `AgentHud.tsx` | The "who is working for me right now" panel already exists and already renders exactly this shape |
| `review_brief` + the VERDICT-line fallback (`agents/mod.rs`) | The pattern for briefing a machine-issued turn and for not trusting the model to format its own conclusion |
| `device.rs::capabilities()` | Each machine already advertises which agent CLIs it has |
| `Capability` + `mesh.onBehalfOf` narrowing (SEC-012) | Authority that travels with the dispatch instead of being re-granted at the far end |

The genuinely new parts are a ledger, a result contract, a tool surface, a
report-back RPC, and the safety envelope. Not an execution engine.

## The shape decision: a child THREAD, not a lane

Parley models multiple agents as **lanes in one thread**, and Parley Phase 4
deferred concurrency for two concrete reasons: `artifact_baselines` is keyed by
thread (`agents/mod.rs:328`), so two writing lanes corrupt artifact detection,
and two agents editing one working tree clobber each other.

A dispatch is a **separate thread**. It gets its own event log, its own driver,
its own artifact baseline, its own working directory and its own machine. Every
reason lanes cannot run concurrently evaporates, and the remote case is free
because remote *threads* are the shipped mechanism.

The cost is that the worker's reasoning is not inline in the parent transcript.
That is the correct trade: a 40-turn Codex implementation session inlined into
the planner's feed is unreadable, and the planner's context budget
(`MAX_TOTAL` = 48k chars, `transcript.rs:17`) cannot absorb it anyway. The
parent gets a report; the child thread stays one click away and fully
inspectable, which is the whole point of Threadknot over an opaque subagent.

**Parley and Dispatch are complementary and should stay separate.** Parley is
*argument* — many voices, one transcript, one working tree, converging on a
verdict. Dispatch is *work* — one brief, one worker, one deliverable, its own
tree. Conflating them gives you a debate that edits files or a build that has to
justify itself.

---

## Data model

### `DispatchRecord` (`dispatch.rs`, persisted to `dispatches.json`)

```rust
pub struct DispatchRecord {
    pub id: String,
    /// Who asked. The ledger lives on THIS machine — the orchestrator owns it.
    pub parent_thread_id: String,
    pub parent_machine_id: String,
    /// The worker.
    pub child_thread_id: String,
    pub machine_id: String,
    pub project_id: String,
    pub agent: Agent,
    pub settings: ThreadSettings,
    pub brief: String,
    /// Short label for the HUD row: "build: macOS", "implement: auth refactor".
    pub label: String,
    pub status: DispatchStatus,
    pub result: Option<DispatchResult>,
    /// Dispatch depth: 0 for one issued by a human-driven thread. Capped.
    pub depth: u8,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

pub enum DispatchStatus { Queued, Running, Succeeded, Failed, Cancelled, TimedOut }

pub struct DispatchResult {
    pub summary: String,
    /// Artifact ids on the WORKER's machine; renderable here via the byte proxy.
    pub artifacts: Vec<DispatchArtifact>,   // { id, name, machineId, mimeType, sizeBytes }
    /// Files the worker says it changed, for the planner's benefit.
    pub changed: Vec<String>,
    /// Populated when the worker never called `report_result` and the summary
    /// was salvaged from its last assistant message.
    pub inferred: bool,
}
```

### The only change to `Thread`

```rust
/// Set when this thread exists because another thread dispatched work to it.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub dispatch: Option<DispatchOrigin>,   // { id, parentThreadId, parentMachineId, label }
```

That is deliberately the whole footprint on the core type. Everything else lives
in the ledger, which means an install with no dispatches carries no new state,
and a `Thread` deserialized by an older build ignores one optional field.

### Events: reuse the subagent trio, don't invent a parallel one

`SubagentStarted` / `SubagentProgress` / `SubagentCompleted` already describe
"a child is working, here is what it's doing, here is how it ended", and
`AgentHud.tsx` already renders them with live activity, elapsed time and result.
Three additive fields make them carry a dispatch:

```rust
SubagentStarted {
    task_id,                 // == dispatch id
    tool_use_id: Option<String>,  // ← becomes optional: a UI-initiated dispatch has no tool call
    description, subagent_type, background, prompt,
    machine_id: Option<String>,   // ← new: renders as a machine chip
    agent: Option<Agent>,         // ← new: renders as "Codex"
    child_thread_id: Option<String>, // ← new: makes the HUD row clickable
}
```

The payoff is that a Codex worker on the Mac and a Claude `Task` subagent appear
in the *same* pinned panel, which is what a user means by "what is running for
me". `subagent_type` becomes the discriminator (`"dispatch"`).

---

## Tool surface

Registered in `mcp.rs::tool_specs()`, gated on the thread's dispatch policy so
an ordinary chat's tool list is unchanged.

### The roster — without this the planner is guessing

```
machines() → [{ machineId, name, os, arch, online, agents: ["claude","codex"],
                roots: [{ projectId, path, name }], acceptsDispatch, maxAccess }]
```

Scoped to the parent thread's workspace, so `roots` is "where this project lives
on that machine". A planner that cannot enumerate the fleet will invent machine
ids, and the failure is a confident error message rather than a build.

### The cheap rung: run a command over there

```
run_on_machine(machine, root, command, timeoutSeconds?) → { exitCode, stdout, stderr, durationMs }
```

Deterministic, no agent, no session, streamed into the parent thread as an
ordinary tool card. This is what *"run `cargo build --release` on the Mac"*
actually needs, and routing a whole agent at it would be absurd.

Needs one new RPC, `exec.run`, added to `ROUTABLE`. `term.rs` is a pty for
interactive use; this is capture-and-exit-code. Streamed in chunks over the
peer socket (never held open as a single 30-second request — see below), gated
on `Capability::Terminal`, output bounded like tool output already is.

### The real rung: dispatch an agent

```
dispatch(brief, label, agent?, model?, machine?, root?, access?, isolate?, syncRef?)
    → { dispatchId, childThreadId, machine, agent }        -- returns IMMEDIATELY

dispatch_wait(dispatchIds[], timeoutSeconds?)
    → [{ dispatchId, status, summary?, artifacts?, elapsedMs }]

dispatch_status(dispatchIds?) → same shape, no blocking
dispatch_cancel(dispatchId)
```

**`dispatch` is non-blocking and `dispatch_wait` is bounded, and that is the
single most load-bearing decision in this document.** An MCP tool call is one
HTTP request. Every harness has its own tool timeout (Claude Code's
`MCP_TOOL_TIMEOUT`, Codex's, Kimi's over ACP), none of them agrees with the
others, and a cross-platform release build takes twenty minutes. If a dispatch
blocks, then whether the Mac build "succeeded" is decided by whichever harness
happens to be driving — and the failure mode is a *timeout error returned to a
model whose worker is still running*, which is how you end up with two builds
racing in one checkout.

So: `dispatch_wait` caps at ~120 s, and **"still running" is a normal,
documented answer that the brief tells the model to re-call on.** The model
polls; nothing anywhere holds a socket open across a build.

### The worker's channel back

```
report_result(status, summary, artifacts?, changed?)
```

Available *only* in threads that have `dispatch: Some(_)`. The brief demands it
as the final action. Models forget, so the fallback is the same defensive
posture Parley uses for its VERDICT line: on turn end with no report, synthesize
one from the last assistant message and mark it `inferred: true`, which the
parent's card renders honestly as "no structured report — summary inferred".
A missing report must never look like a failure, and must never look like a
clean success either.

---

## Cross-machine mechanics

**Starting a remote worker needs no new transport.** `dispatch.create` joins
`ROUTABLE`; when it names another machine, the existing forwarder ships it over
the peer socket, the owner creates the child thread in its own store and starts
the turn. That is byte-for-byte what `thread.create` + `turn.start` already do
for remote threads.

**Finishing needs one new machine-to-machine RPC.** The peer request timeout is
30 s (`peernet.rs:31`) and the outbound queue is 64 deep
(`limits::PEER_REQUEST_QUEUE`); holding a request open for the length of a build
would wedge the link for every other user of it. So completion is *pushed*:

```
mesh.dispatchReport { dispatchId, status, summary, artifacts, changed }
```

sent by the worker machine to `parent_machine_id` at the child's turn boundary.
Parent offline ⇒ the report is queued in the worker's ledger and re-sent on
reconnect, reusing the resync-on-connect hook that already re-pushes workspace
state. Progress is the same shape, throttled: `mesh.dispatchProgress` every few
seconds carrying one activity line, which becomes a `SubagentProgress` in the
parent thread.

Progress deliberately does **not** ride the client event relay. That relay
delivers the worker's frames to whichever clients happen to be connected; a
dispatch has to make progress with the UI closed and the phone asleep.

**Artifacts stay where they were built.** The worker registers them normally;
the report carries their ids and machine; the parent thread stores them as
artifact *references* and renders them through `/artifact-file?machineId=`,
which Phase 4.5 already implements. Three builds on three machines end as three
openable cards in one thread, with no copying and no cloud.

## Getting the source code over there

`MULTI-MACHINE.md` is explicit: **no source sync, git is the only code channel.**
That constraint decides the build story's ergonomics, so it gets handled in the
feature rather than left to the model:

- Fanning a build out requires the workspace to have a **root on each machine**
  (`workspace.attachRoot`, shipped). "Build on the Mac" with no Mac root is a
  precondition failure with a fix-it message, not a mysterious empty directory.
- `dispatch(..., syncRef: "HEAD")` runs a prelude before the worker's first
  turn: `git.push` here, `git.pull` there, then **verify the worker's HEAD sha
  matches**, and refuse the dispatch if it doesn't.

That verification is the highest-value detail in the whole build path. Without
it the plausible outcome is a Windows installer of last Tuesday's code that
looks, in every artifact card and every summary, exactly like a correct build.

## Safety envelope

Fanning agents across your machines is the most dangerous thing Threadknot can
do. Every one of these lands with the feature, not after it.

- **`Capability::Dispatch`**, master-only by default. A phone gets `Threads` so
  it can drive a chat; it does not get to fan a Full-access Codex out over the
  fleet. (`Capability::ALL` is a fixed-size array — the compiler will find the
  sites.)
- **Per-machine consent.** `device.json` gains `acceptsDispatch: bool` and
  `maxDispatchAccess: Access`, surfaced in Settings → machines and advertised in
  `device.info`. A machine can be in the mesh — sharing files, showing threads —
  without agreeing to have arbitrary agents driven on it. The knob has to exist
  before the first dispatch, not after the first surprise.
- **Authority only narrows.** `child.access = min(parent.access, requested,
  target.maxDispatchAccess)`, and the routed request carries the originating
  caller's grants via the existing `mesh.onBehalfOf` propagation. There is no
  path by which dispatching widens what the human sitting here could already do.
- **Depth cap, default 1.** A dispatched agent that can itself dispatch is a
  fork bomb with your subscription attached. Depth 2 is configurable for
  planner→lead→worker; unbounded is never available.
- **Concurrency and count caps** in `limits.rs`: max in-flight dispatches per
  machine, per parent thread, and per turn.
- **Cost is visible before it is spent.** Three workers against three
  subscription windows is real money and real rate limit; `usage.rs` already
  tracks the windows, and Parley already established showing the estimate up
  front.
- **Working-tree collisions.** Two dispatches into the same `(machine,
  projectId)` clobber each other — the exact hazard Parley Phase 4 named.
  Default: serialize per `(machine, projectId)`. `isolate: "worktree"` runs the
  worker in `git worktree add .threadknot/worktrees/<dispatchId>`; `git.rs`
  already understands linked worktrees (`looks_like_repo` resolves `gitdir:`
  pointers into `.git/worktrees/`). Different machines never collide, which is
  the common case for the build fan-out.
- **Nothing is orphaned.** `recover_orphaned_threads` already reconciles threads
  whose driver died across a restart; child threads inherit it, and a dispatch
  whose child is gone resolves as `Failed` rather than `Running` forever.

## UI

- **AgentHud** (the pinned agent panel) is the primary surface — dispatched
  workers as rows with a machine chip and an agent chip, live activity, elapsed
  time, result, and a click-through to the child thread. It renders this shape
  today.
- **Crew card** in the parent feed for a fan-out: one card, one row per target,
  live status per row, artifacts attaching as they land, and an honest aggregate
  ("2 of 3 succeeded — Windows failed at signing"). A fan-out that reports only
  its successes is worse than no fan-out.
- **Child threads are visible, nested under the parent** in the sidebar with a
  "dispatched" chip, collapsible. Hiding them would trade away the thing
  Threadknot has that a subagent API doesn't: the work is inspectable.
- **A preset for ask 1 without prompting for it.** "Plan with Opus, build with
  Codex" as a thread preset that sets the builder's default dispatch target, so
  the common case is a dropdown rather than a paragraph of instructions.

---

## Phases

Each phase is independently shippable and independently useful.

**Phase 0 — run a command on another machine.**
`exec.run` (routable, chunk-streamed, exit code, bounded output) + the
`run_on_machine` and `machines()` MCP tools. No ledger, no child threads.
*Gate:* from a thread on Arch, compile a real binary on the Mac and see the log
inline with an exit code. This alone covers most of ask 2.

**Phase 1 — local dispatch.**
`dispatch.rs` ledger, child threads, `Thread.dispatch`, the dispatch brief,
`report_result` + inferred fallback, `dispatch`/`dispatch_wait`/`_status`/
`_cancel`, the subagent-event extensions, HUD rows, depth and concurrency caps,
worktree isolation. `machineId` is always self.
*Gate:* Opus plans, dispatches Codex and Kimi in parallel on this machine, reads
both reports, and the parent transcript states what each one did. Ask 1, done.

**Phase 2 — dispatch across the mesh.**
`dispatch.create` routable; `mesh.dispatchReport` / `mesh.dispatchProgress` with
the offline queue and resync-on-reconnect; remote artifact references;
`Capability::Dispatch`; per-machine consent and access cap.
*Gate:* from Arch, dispatch three builds (Arch, Windows, Mac); all three report
back; three artifacts open from the parent thread. Ask 2, done.

**Phase 3 — fan-out sugar.**
`dispatch` accepting `machines: [...]` or `targets: "workspace-roots"`; the crew
card; the `syncRef` prelude (push → pull → verify sha → refuse on mismatch);
failure aggregation; presets.
*Gate:* one sentence — *"make me a production build for all three machines"* —
produces three verified builds or a precise account of which one failed and why.

**Phase 4 — reach.**
Dispatch from `Schedule` (nightly cross-platform build), reusable dispatch
templates, push notification when a dispatch finishes while you're away,
dispatch from a Hermes gateway agent, dispatch as a Parley execution step.

The first two shipped. The schedule keeps an optional `ScheduleDispatch` block
(`protocol.rs`) and `schedules::fire_dispatch` opens a **coordinator thread**
that runs no turn of its own — it exists to hold the crew, own the ledger
entries, and give the workers something to report into. Each target goes through
`dispatch.create` rather than the ledger directly, so a scheduled dispatch gets
exactly the checks an agent-issued one does: target consent, agent-installed,
access narrowing, depth and concurrency caps.

Three decisions worth keeping:

- **`Capability::Threads` is not enough.** Every `schedule.*` kind maps to
  `Threads`, which is right for a schedule that starts a turn and far too weak
  for one that runs code on three machines on a timer. `require_dispatch_authority`
  adds `Terminal` at create, update *and* `schedule.run` — checking it only at
  write time would leave "Run now" as the bypass.
- **Partial failure is a first-class outcome.** Some workers started → a
  `Status` line naming who refused (an `Error` there would settle the thread out
  from under the workers still running). None started → an `Error`, and the
  schedule's `lastError` says why. A nightly build that quietly ran on two
  machines out of three is worse than one that ran on none.
- **Somebody has to settle the coordinator.** It never runs a turn, so no
  `TurnCompleted` is coming; `settle_if_idle` runs as each report lands and
  marks it idle once no dispatch is live and no session is attached. A no-op for
  an agent-issued dispatch, whose parent settled when its own turn ended.

## Testing strategy

Two instances on this box (`THREADKNOT_PORT` + `THREADKNOT_DATA_DIR`) driven by
the headless smoke script, extended: pair → attach a root on each → dispatch a
real Codex turn from A onto B → assert progress lands in A's parent thread →
assert the report arrives → kill A mid-dispatch and assert the report is queued
and delivered on reconnect → assert an artifact produced on B opens from A.
The Mac and the Windows machine are the integration test at the end of Phase 2;
the honest version of that test is a real `npx tauri build` on each.

## Decisions log

- Dispatch is a **child thread**, not a Parley lane — per-thread artifact
  baselines and one-working-tree-per-thread are why lanes cannot be concurrent,
  and both are sidestepped rather than fixed.
- `dispatch` returns immediately; `dispatch_wait` is bounded and "still running"
  is a valid answer. No harness's MCP tool timeout gets to decide whether a
  twenty-minute build succeeded.
- Completion is **pushed** worker → parent, never awaited on a peer request. The
  30 s peer timeout and the 64-deep queue are shared infrastructure.
- Progress does not ride the client event relay: a dispatch must make progress
  with every UI closed.
- Subagent events are **extended**, not duplicated — one panel answers "what is
  running for me", whether it's a Claude `Task` or Codex on the Mac.
- No source sync. Git stays the only code channel, and `syncRef` verifies the
  sha rather than hoping.
- Per-machine dispatch consent ships with the feature. Mesh membership is not
  consent to have arbitrary agents driven on your laptop.

## Open questions

1. **Depth cap default — 1 or 2?** 2 enables planner → lead → workers, which is
   the natural shape for a large feature, and doubles the blast radius.
2. **Should a dispatched worker inherit the parent's Library skills and MCP
   servers?** The Library is per-machine by design; a worker on the Mac gets the
   Mac's shelf, which may not be the one the brief assumes.
3. **Cross-provider brief translation.** Codex and Kimi read a Claude-authored
   brief fine, but `transcript::HEADER` exists because handoffs need framing.
   Does a dispatch brief need per-provider phrasing, or is one brief enough?
4. **Does the parent stay Running while workers work?** Blocking the planner is
   simple and wastes a subscription window; letting it continue means it may
   speak about work that has since changed underneath it.
5. **Failure policy for a fan-out** — fail fast and cancel siblings, or always
   let all targets finish? (Proposal: always finish; a partial matrix is
   information, and cancelling a 15-minute Mac build because Windows failed at
   minute two is the wrong trade.)
