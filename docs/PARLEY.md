# Parley — multi-agent adversarial review

*Parley*: a conference between opposing parties. Two or more agents — different
providers, different models, or the same model twice — work the same thread as
named participants, review each other's plans and code, and argue to a verdict.
(Name is cheap to change; `council`/`roundtable` were the alternatives.)

Two entry points, one mechanism:

- **Throw a reviewer at an existing thread.** Claude planned something; add
  Codex as a reviewer and it argues the plan.
- **Start a thread with a panel.** Builder + reviewer(s) named up front; every
  turn is followed by review before the user sees a verdict.

## Why this is mostly already built

Threadknot's mid-thread agent switch (Traycer-style handoff) is the debate primitive:

| Existing piece | What Parley needs it for |
| --- | --- |
| Provider-agnostic event log (`store::read_events`) | The shared transcript every participant reads |
| `Thread.session_anchors` + `covered_until_seq` (`protocol.rs:44`) | Per-participant "how much have I absorbed" |
| `transcript::render(events, after_seq)` (`transcript.rs:45`) | Each participant's turn input: exactly the delta it hasn't seen, with speaker labels already in it (`[assistant — Codex · gpt-5]`) |
| `transcript::HEADER` | Already tells a joining model that earlier turns may have been another assistant's and to treat them as ground truth |
| `ThreadSettings.access` (`protocol.rs:85`) | Reviewer permissions: full control by default, read/edits as the opt-in "ask me" mode |
| MCP tool channel (`mcp.rs`, `publish_artifact`) | Structured debate moves |
| Subagent HUD (`AgentHud.tsx`, `SubagentInfo`) | The "who's running now" pattern to reuse for lanes |

The handoff seed makes participant B read participant A's work as ordinary
conversation history, no new context plumbing. A debate is that seed, alternating,
with role prompts and a scheduler.

## Architecture: one room, N lanes, deterministic moderator

One thread is the **room**. It owns the single ordered event log — the debate
transcript. Each participant is a **lane**: its own provider session, its own
anchor into the shared log, its own access level.

Turn-taking is a **Rust state machine, not an LLM**. A meta-agent deciding who
speaks next burns a turn per decision and can't be debugged. Rounds are fixed,
one speaker at a time, with explicit stop conditions.

### Data model (`protocol.rs`)

```rust
pub struct Participant {
    pub id: String,              // lane id; keys anchors, sessions, events
    pub agent: Agent,
    pub settings: ThreadSettings, // per-lane model/effort/access — reviewers default to Access::Read
    pub role: ParticipantRole,   // Builder | Reviewer | Arbiter
    pub name: String,            // "Codex (reviewer)" — user-renameable
    pub color: String,           // lane color; drives every view
    pub image: Option<String>,   // reuse the Hermes/Claudex avatar path
}
```

- `Thread.participants: Vec<Participant>`.
- **Re-key `session_anchors` from `Agent` to participant id.** This is what makes
  "the same model twice" work — two Claude lanes need two anchors, and today the
  map physically cannot hold them. Migration wraps each existing thread as one
  participant whose id is the stringified agent, so old anchors keep resolving.
- `Thread.active_speaker: Option<String>` — which lane is mid-turn. `Thread.status`
  stays a single aggregate: with one speaker at a time, `Running` still means
  "something is running in this room", so none of the 32 existing `ThreadStatus::`
  sites nor `statusFromEvent` (`store.tsx:240`) need to change.

### Attribution: `speaker` on the event *envelope*

```rust
pub struct PersistedEvent {
    pub seq: u64,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,   // participant id; None = the user, or a pre-Parley thread
    pub event: AgentEvent,
}
```

On the envelope, **not** on each `AgentEvent` variant — one change and every kind
(tool calls, thinking, diffs, artifacts, approvals) becomes attributable for free.
`ServerMessage::Event` gains the same field. Without this there is nothing to
visualize, so it lands in Phase 1 even though nothing renders lanes yet.

`DriverCtx` (`agents/mod.rs:1446`) carries the participant id and stamps it on
every emit, so drivers need no per-call changes.

### Structured debate moves (`mcp.rs`)

Prose-only debate can only ever render as prose, and models asked to "critique"
produce agreeable mush. Typed moves, same registration pattern as
`publish_artifact`, gated to threads with >1 participant:

| Tool | Args |
| --- | --- |
| `raise_finding` | `severity` (blocker/major/minor/nit), `claim`, `evidence` (file:line, command output), `target` (participant id or `plan`) |
| `respond_finding` | `finding_id`, `stance` (agree/dispute/partial), `rationale` |
| `revise` | `finding_id`, `what_changed` |
| `concede` | `finding_id`, `why` |
| `escalate` | `finding_id`, `question` — the models disagree and need the user |

Findings persist as `FindingRecord` alongside `ArtifactRecord` in `store.rs`, and
emit `AgentEvent::Finding` / `FindingResponse` so they render inline *and*
aggregate into the ledger. **This is the source of every view below** — the graph
is the data, not a layout.

Prompt discipline that matters: reviewers are told to open with the strongest
objection they can defend with evidence, to say "no material objection" rather
than manufacture one, and that conceding a finding is a valid, cheap move.
Otherwise round 2 is two models complimenting each other.

### The moderator (`council.rs`)

```
start_parley(thread, rounds, stop) →
  for round in 1..=rounds:
    for lane in schedule(round):            // builder first, then reviewers
      seed = transcript::render(events, lane.anchor.covered_until_seq)
      prompt = role_prompt(lane, round) + open_findings_digest()
      run one turn on lane; advance its anchor
    if converged() { break }                // no new findings, or all resolved
  emit verdict
```

- **Write baton (phase 4 invariant).** Today turns are strictly sequential, so
  the baton is not load-bearing: reviewers default to FULL control — a reviewer
  that must ask permission for every command stalls the debate on a human
  click — and `read`/`edits` is the deliberate opt-in restriction. The HARD
  version of this rule — exactly one lane holds `Edits`/`Full` — only becomes
  load-bearing with concurrent lanes: `artifact_baselines` is keyed by thread
  (`agents/mod.rs:328`), so two writing lanes would corrupt artifact detection,
  and two agents editing one working tree clobber each other. Git worktrees
  per writer stay open as a Phase 4 option.
- **Stop conditions:** round cap (default 2), convergence (a full round adding no
  new findings), any `escalate`, or user interrupt. Never unbounded.
- **Context budget.** `MAX_TOTAL` is 48k chars (`transcript.rs:17`) and a 3-lane
  debate blows past it. Per-lane budget, and rounds older than the last get
  replaced in the seed by a findings digest rather than full prose.
- **Cost is visible up front:** 3 lanes × 2 rounds = 6 turns against the
  subscription windows `usage.rs` already tracks. Show the estimate before starting.

### Protocol additions (`server.rs`)

`thread.addParticipant` · `thread.removeParticipant` · `thread.setParticipant`
(rename/color/model/access) · `parley.start` · `parley.stop` ·
`thread.review` (sugar: add a reviewer + run one review round) ·
`findings.list` · `finding.resolve` (user's own verdict on an escalation).

## Visualization

Four views over one dataset. The ledger is the one that earns its keep.

**Lanes** (desktop default) — vertical swimlanes, one per participant in its
color. User messages span full width as round dividers. Cross-references draw as
arrows lane-to-lane: Codex's finding → Claude's rebuttal → Codex's concession.
Time flows down. A lane mid-turn pulses; its tool calls collapse to one line so
the argument stays readable.

**Disagreement ledger** (the money view) — findings as rows, grouped
Blocker/Major/Minor, columns for each participant's stance (✓ agree · ✗ dispute ·
~ partial · — silent), state as Open / Disputed / Conceded / Resolved /
**Needs you**. Sort by severity, filter to unresolved. This is what you read after
a parley instead of scrolling two transcripts.

**Consensus header** — `7 findings · 4 agreed · 2 disputed · 1 needs you`, round
dots showing who spoke and who's live (the `AgentHud` pattern), and the running
turn cost.

**Verdict card** — posted into the feed at parley end: what changed in the plan,
what was conceded, what's still open and escalated to the user, with the
escalations as answerable cards (reuse `QuestionCard.tsx`).

Mobile: ledger first, lanes collapse to a single feed with colored speaker chips —
swimlanes don't survive a phone viewport.

## Phasing

**Phase 1 — one-shot review. SHIPPED.** What landed:

| Piece | Where |
| --- | --- |
| `Participant`, `ParticipantRole`, `Thread::participants_resolved` / `primary_participant` / `speaking_participant` | `protocol.rs` |
| `sessionAnchors` re-keyed `Agent` → participant id (no migration needed: an anchor's JSON key was already the agent's wire name, which is the implicit builder's id) | `protocol.rs`, `agents/mod.rs` |
| `speaker` on `PersistedEvent` + `ServerMessage::Event`; `DriverCtx.participant_id` stamps every driver emit via `Hub::emit_as` | `protocol.rs`, `agents/mod.rs`, `store.rs` |
| Session key + anchor lookup scoped to a lane, so two lanes on the same agent never share a process | `session_key` / `usable_anchor` in `agents/mod.rs` |
| `Hub::join_reviewer` (seat a lane) + `Hub::review` (seat + one turn), `start_turn_as`, the reviewer-yields-the-floor rule at the turn boundary | `agents/mod.rs` |
| `review_brief` — the role prompt (evidence required, severity ranking, "no material objection" explicitly permitted). The permissions paragraph tracks the lane's access: read-only lanes are forbidden from editing; a lane granted write access is told to report first, patch second | `agents/mod.rs` |
| `injected` on `user_message`; the seed labels it `[brief issued to another assistant]` and lane names disambiguate same-model lanes | `protocol.rs`, `agents/transcript.rs` |
| `thread.review` RPC (routable, so remote threads review on their owner machine) | `server.rs` |
| Speaker chips, collapsed brief divider, lane roster, "Review with…" dialog — a portaled modal (the feed could paint over the original anchored popover) with full reviewer settings: agent(s), per-reviewer model/effort, access (default FULL control — restricted modes warn that the reviewer will pause the debate to ask), rounds, execute toggle, focus; the last setup is remembered | `FeedItems.tsx`, `ReviewMenu.tsx`, `ThreadView.tsx`, `feed.ts`, `styles.css` |

Deliberately NOT in phase 1: concurrency, the moderator/rounds, structured
findings, the ledger. The composer always addresses the builder; to hear from a
reviewer again you run another review (which reuses its lane and session).

**Phase 2 — debates. SHIPPED.** The monologue became a conversation:

| Piece | Where |
| --- | --- |
| `ParleyState` on `Thread` (`lanes`, `round`, `maxRounds`, `next`, `objectors`, `hadObjections`, `execute`, `pendingUser?`, `inFlight?`) — persisted so every client sees the same round and the scheduler reads its own state back at each boundary | `protocol.rs` |
| `parley_decide` — the deterministic round scheduler (pure function, fully unit-tested): score the finished turn, seat the next speaker. Never an LLM | `agents/mod.rs` |
| `Hub::start_parley` + `Hub::advance_parley` (driven from the `emit_as` turn boundary via the hub's cyclic weak self-ref), `end_parley_with_note` | `agents/mod.rs` |
| Verdict parsing: reviewer briefs demand a final `VERDICT: CONCEDED \| OBJECTING` line (phrase fallback; missing marker counts as objecting, so a confused model extends the debate instead of ending it) | `agents/mod.rs` |
| Convergence → optional execution turn ("implement exactly what you conceded"); **every ending runs ONE closing builder turn so the thread is left with a deliverable, never a dangling "so what's the plan?"** — `plan` (the agreed plan, numbered and concrete), `escalation` (settled vs. open, each side's position, the exact questions the user must answer) | `agents/mod.rs` |
| User interjection: `turn.start` mid-parley queues onto `parley.pendingUser` and takes the floor at the next boundary instead of failing the idle check; `turn.steer` still reaches the current speaker | `agents/mod.rs` |
| Fatal driver `error` now restores the floor to the builder (a dead reviewer used to leave its identity and settings on the thread) | `agents/mod.rs` |
| **Personas** — named, reusable reviewer presets (`personas.json`, ride `hello`, edited via `persona.save`/`persona.delete` from any authenticated client). A persona is a display name (which becomes the lane name) + the agent that powers it + a personality folded into every brief. Three built-ins (The Skeptic / Second Opinion / The Contrarian) seed on first run; the dialog's default panel is every online persona, so a first debate is one click | `personas.rs`, `server.rs`, `agents/mod.rs`, `ReviewMenu.tsx` |
| `thread.parley.start` RPC (routable); review dialog is multi-reviewer with per-reviewer model/effort, shared access, rounds, and an execute toggle | `server.rs`, `ReviewMenu.tsx`, `protocol.ts`, `App.tsx` |

Deliberately NOT in phase 2: structured findings (the debate is still prose +
verdict lines), the ledger, lanes view, concurrency, per-lane context budgets
(a 3-lane debate can blow the 48k seed budget — the moderator design's
findings digest is the planned answer).

**Phase 3 — the argument made of data.** Findings MCP tools + `FindingRecord` +
moderator digest + ledger + verdict card + escalation cards. Panel threads from
scratch (builder + reviewer named at creation, optionally as a `Schedule`). This
replaces VERDICT-line parsing with real structure.

**Phase 4 — parallelism and reach.** Concurrent lanes (needs per-lane artifact
baselines and per-lane status), git worktrees for multiple writers, participants
running on peer machines (the mesh already carries remote threads), convergence
tuning.

## Verify gate

`cargo build && cargo clippy` in `src-tauri/`, `npm run build`. Smoke: drive
`threadknot-headless` over `/ws` with a two-lane thread — assert each lane's anchor
advances independently, that a re-keyed legacy thread still resumes natively, and
that a reviewer lane gets write tools only when `thread.review` granted them.
