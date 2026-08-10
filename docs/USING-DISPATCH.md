# Using Dispatch — handing work to another agent, on another machine

> This is the **operator's guide**: what Dispatch does and how to drive it.
> `DISPATCH.md` is the design record — why it is a child thread rather than a
> Parley lane, why the result contract looks like it does, what is still
> unbuilt. When the two disagree, the code wins and this file is stale; keep
> usage here and rationale there rather than growing a second copy of either.

A **dispatch** hands one self-contained job to another agent — a different
harness, a different model, a different machine — and gets a written report
back. The agent doing the work gets its own thread, its own context and its own
working directory. It cannot see the conversation that sent it.

That single operation covers two things that look unrelated:

- *"Plan this with Opus, but let Codex write it."* — vary the **agent**.
- *"Build me a release for Linux, Windows and macOS."* — vary the **machine**.

And the combination falls out for free: Codex does the Windows build while Kimi
does the Mac.

---

## Starting one

There are two doors. Both end in the same ledger.

### 1. An agent calls the `dispatch` tool

This is the normal path. Any thread with the fleet tools available can delegate:

```
dispatch(
  brief:    "Pull master, run a release build, report the version and where the binary landed.",
  label:    "release build",
  machines: ["spencer_windows", "Mollys-MacBook-Pro-2.local"],
  agent:    "codex",
  access:   "full",
  syncRef:  true,
)
```

It returns **immediately** with dispatch ids — the work continues in the
background. Collect results with `dispatch_wait`, which answers within ~25
seconds whether or not anything has finished; `"running"` is a normal answer and
you call it again. A build can take half an hour, and no tool call is going to
sit still for that.

The companion tools:

| Tool | What it is for |
| --- | --- |
| `machines` | The roster: which machines exist, their OS, whether they are reachable, which agent CLIs each has, and which of this workspace's roots live on each. Call this first. |
| `dispatch` | Hand over a brief. `machines: [...]` fans one brief out to several, one worker each. |
| `dispatch_wait` | Wait for reports. Call repeatedly. |
| `dispatch_status` | Snapshot without waiting. |
| `dispatch_cancel` | Stop one (`dispatchId`) or several (`dispatchIds`). Final — a late report cannot resurrect a cancelled dispatch. |
| `run_on_machine` | The cheaper rung. See below. |
| `report_result` | Only offered *to* a worker. How it answers. |

### 2. A schedule

**Settings → scheduled runs → New scheduled run**, then switch *Each run…* from
**Runs here** to **Dispatches to machines**.

The form then asks for the machines (a chip per machine, including this one),
optionally which root to use on each, and whether to sync the commit first. The
brief is the same prompt field, relabelled.

Each firing opens a **coordinator thread** — it runs no turn of its own, it
holds the crew. The brief lands in it, one worker goes out per machine, and
every worker reports back into it. That thread is what you open the next morning
to see how the nightly build went.

Because a dispatching schedule runs code on other machines unattended, saving
*or running* one needs the **Terminal** grant in addition to Threads. A
phone with only Threads can create an ordinary schedule and will be refused a
dispatching one.

> A **disabled** dispatching schedule plus **Run now** is a saved, one-click,
> fully-specified dispatch — which is most of what a reusable template is for.
> That is deliberate; there is no separate template registry.

### There is no button for a one-off dispatch

Today you either ask an agent to delegate, or you save a schedule. A "dispatch
this to X" control in the UI is not built.

---

## What the worker actually receives

Not your conversation. It wakes up to a generated brief that says:

- it is a dispatched worker, and which thread on which machine sent it;
- its one-line job label;
- your brief, verbatim;
- **how to finish**: call `report_result` exactly once, as its final action.

The brief tells it plainly that the report *is the whole channel* — the sender
does not read the worker's thread — and that reporting failure honestly beats an
optimistic summary. It is also told not to ask questions, because nobody can
answer, and to state which interpretation it took if the brief was ambiguous.

If the worker is running below full access, the brief names the restriction, so
it reports what it could not do instead of trying to work around it.

A worker that produces a file should `publish_artifact` it first; artifacts are
attached to the report automatically.

**If a worker never calls `report_result`**, the turn boundary infers one from
its last message and marks the report `inferred`. `dispatch_wait` flags that
explicitly — treat an inferred summary as a hint, not an outcome, and open the
thread.

---

## Access, and the one trap in it

A dispatch can only ever **narrow**. The worker's access is the minimum of three
things:

1. what the sender asked for,
2. what the sending thread itself holds,
3. what the target machine will accept.

There is no request a parent can make that grants a worker more than the parent
has. Mesh membership is not consent to have arbitrary agents run at full tilt on
your laptop.

**The trap: `read` and `edits` do not mean what they mean in a chat.** In a chat
they mean "ask me first" — a card comes up and you approve it. A dispatched
worker has nobody to ask. So:

- at `access: "read"`, **every command it tries to run is refused**;
- at `access: "edits"`, every write is refused.

The worker is told this, adapts, and reports a list of things it could not do —
so it finishes rather than hanging. (Before this was handled, such a worker sat
in *waiting approval* forever.) But the practical rule is:

> **Use `full` for work you actually want done unattended, and keep the *brief*
> read-only if read-only is what you meant.**

Two more things a worker never inherits: your **signed-in browser profile**
(that is a credential, and "do this build" does not imply handing it over), and
your **model id** when the harness differs — a Codex worker handed
`claude-opus-5` fails at the API with a message about ChatGPT accounts that
names nothing real, so it falls back to that agent's own default.

Workers always run in **Build** mode. Plan mode would have them raise
plan-approval cards at a sender that cannot answer them.

### Per-machine consent

Each machine decides what it will accept, in its own `device.json`:

- `acceptsDispatch` — refuse dispatched work entirely. Absent means yes; a build
  that predates the setting never declined.
- `maxDispatchAccess` — the ceiling every incoming worker is narrowed to.

A machine is *asked* what it accepts (`device.info`) rather than assumed, so an
offline peer answers nothing, which is the refusal.

**There is no UI for these yet** — they are hand-edited. That is a real gap for
a security control.

---

## Where it shows up

**In the sidebar**, dispatched workers nest under the thread that sent them,
behind one collapsed line: *"3 workers · 2 running"*. Collapsed by default,
because a fan-out to three machines should cost one row, not four. It opens
itself while you are searching, when one of the workers is the thread you have
selected, or when one wants attention. Clicking a worker opens its thread —
they are real threads and fully inspectable, which is the thing Threadknot has
that a subagent API does not.

**In the agent panel** of the sending thread, one row per worker with a machine
chip, an agent chip, live activity, and a link through to its thread.

**Progress** normally streams from the worker as it works. When the return link
is one-way — which happens for real, notably an unsigned macOS binary launched
over SSH, where outbound LAN connects fail silently — the sender notices the
silence after ~7s and starts polling every 2s instead, carrying the worker's
last activity line. You get progress either way.

**A notification** fires when a worker reports. This is the whole point of
unattended dispatch: a cross-platform build that finishes at 03:00 has nothing
else that can tell you.

---

## Fan-out and honest failure

`machines: [...]` sends the same brief to each target, one worker apiece, and
labels the rows per machine so they are distinguishable.

Partial failure is always reported, never swallowed:

- **Some workers started** — the run continues, and the targets that refused are
  named (in a `dispatch` call, a `refused` array; in a scheduled run, a status
  line in the coordinator thread saying "2 of 3 workers started" and why).
- **No worker started** — the run fails outright, with the reason recorded.

A nightly build that quietly ran on two machines out of three is worse than one
that ran on none, because the missing platform gets discovered by a user.

### `syncRef`

Set it for anything you intend to compare across machines. It brings each remote
checkout to the **same commit** as the sender first, and **refuses the dispatch**
if it cannot — usually because the commit is not pushed yet. Without it you can
get a build of stale code that looks entirely successful.

---

## The cheaper rung: `run_on_machine`

If you know the exact command and no judgement is involved, do not spend an
agent on it:

```
run_on_machine(machine: "spencer_windows", command: "cargo test --release")
```

It runs through `sh` on Linux/macOS and **PowerShell on Windows** — check the OS
with `machines` before writing shell syntax. It answers within ~25 seconds; if
the command outlives that you get a `jobId` and poll it with `exec_status`. The
job keeps running regardless of whether anyone is watching, and cancelling kills
the process group rather than just the shell.

Commands run in a registered workspace root, and a `subdir` that escapes that
root is refused rather than guessed at.

Use `dispatch` instead when the job needs judgement or may have to fix its own
failures.

---

## Limits

| | |
| --- | --- |
| Dispatch depth | **1** — a worker cannot dispatch further work. It reports back instead. |
| Live workers per sending thread | 8 |
| Live workers per machine | 4 |
| Remembered dispatches | 200 (finished ones trimmed; running ones never) |
| Report summary | 8 000 chars |
| Command default / max timeout | 10 min / 3 h |
| Concurrent commands per machine | 8 |

Concurrent dispatches into the **same checkout** are serialized, not run in
parallel — correct, but slower than the design intends. Worktree isolation is
not built.

---

## When something goes wrong

| Symptom | Cause |
| --- | --- |
| Worker reports a list of things it "could not do" | It was dispatched below `full` access. Nothing is broken; see above. |
| `that machine is not accepting dispatched work` | `acceptsDispatch: false` in its `device.json`. |
| `that machine does not have X installed` | The roster from `machines` lists what each has. |
| `N has 0 roots in this workspace` | The target contributes no root. Attach one to the workspace first. |
| `no machine matches "..."` | Names come from `machines`. An ambiguous prefix fails loudly rather than picking one — a build landing on the wrong machine is not something you notice for a week. |
| Dispatch refused with a commit mismatch | `syncRef` doing its job. Push first. |
| Worker on a Mac cannot authenticate Claude | macOS keeps Claude's credentials in the login Keychain, which a non-GUI session cannot unlock. Use **Codex** or **Kimi** there — they use file auth. |
| Report marked `inferred` | The worker never called `report_result`. Open its thread. |
| A paired machine will not come online | If it was paired before the encrypted mesh, it is *refused*, not offline. Update Threadknot there and pair again. |

---

## Where the code lives

| | |
| --- | --- |
| Ledger, lifecycle, briefs, mesh RPCs | `src-tauri/src/dispatch.rs` |
| Agent-facing tools | `src-tauri/src/mcp_fleet.rs` |
| Non-interactive commands | `src-tauri/src/exec.rs` |
| Scheduled dispatch | `src-tauri/src/schedules.rs` |
| Caps and intervals | `src-tauri/src/limits.rs` |
| Per-machine consent | `src-tauri/src/device.rs` |
| Sidebar nesting, schedule form | `src/components/Sidebar.tsx`, `src/components/SchedulesPanel.tsx` |

Proofs: `scripts/dispatch-smoke.py` (two paired instances — routed exec, offline
report replay, cancel-is-terminal), `scripts/dispatch-live.py` (real Codex and
Kimi workers), `scripts/dispatch-crossos.py` (Linux ↔ macOS with the return link
down), `scripts/schedule-dispatch-smoke.py` (a schedule fanning out unattended).
