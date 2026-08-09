//! Shared client<->server protocol types. Contract: docs/PROTOCOL.md.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    /// Kimi Code CLI driven over its official ACP stdio protocol. Authentication
    /// and usage come from the user's Kimi subscription (`kimi login`).
    Kimi,
    /// Remote Hermes Agent gateways (Nous Research hermes-agent). One agent
    /// KIND for all registered gateways; the specific agent (profile) rides in
    /// `ThreadSettings.model` as the registry entry's id.
    Hermes,
    /// The Claude Code harness driven by a NON-Anthropic model through an
    /// Anthropic-compatible gateway (see `claudex.rs`). Same driver as
    /// `Claude`, different process environment — so it gets its own agent kind
    /// to keep sessions, anchors and live drivers from being shared with a
    /// genuinely Anthropic-authenticated one. Like Hermes, the specific
    /// profile rides in `ThreadSettings.model` as the registry entry's id.
    Claudex,
}

impl Agent {
    pub fn display_name(&self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Kimi => "Kimi Code",
            Agent::Hermes => "Hermes",
            Agent::Claudex => "Claudex",
        }
    }

    /// The agent's wire name — identical to its serde representation, which is
    /// also how it appeared as a `session_anchors` map key before anchors were
    /// re-keyed by participant id. Using it as the implicit participant's id is
    /// what lets pre-Parley anchors keep resolving with no migration.
    pub fn wire_id(&self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Kimi => "kimi",
            Agent::Hermes => "hermes",
            Agent::Claudex => "claudex",
        }
    }

    /// Does this agent kind run the `claude` CLI? Both Claude-driven kinds
    /// share `agents::claude`, and differ only in the child's environment.
    pub fn is_claude_cli(&self) -> bool {
        matches!(self, Agent::Claude | Agent::Claudex)
    }
}

/// What a participant is in the room for. Drives the role prompt it gets and
/// the default access it runs with; a Reviewer is read-only and never sticks
/// (the turn after its review routes back to the Builder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParticipantRole {
    /// Does the work. Exactly one per thread holds the write baton.
    Builder,
    /// Argues with the work. Read-only.
    Reviewer,
}

/// One lane in a thread: a provider session with its own identity, model and
/// access level, anchored into the thread's single shared event log.
///
/// A thread with no explicit participants behaves exactly as it did before
/// Parley — see [`Thread::participants_resolved`], which synthesizes the
/// implicit Builder from `Thread::agent`/`settings`. Two participants may share
/// an agent kind and model (Claude reviewing Claude); they are distinguished by
/// `id`, which is what keys both `session_anchors` and the live driver.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    /// Lane id. For the implicit Builder this is [`Agent::wire_id`] so legacy
    /// anchors keep resolving; added participants get a fresh uuid.
    pub id: String,
    pub agent: Agent,
    /// Per-lane model/effort/access. Reviewers run `Mode::Build` always and
    /// default to [`Access::Full`] (hands-off) unless the client restricts them.
    pub settings: ThreadSettings,
    pub role: ParticipantRole,
    /// The persona (`personas.json` id) this lane belongs to, if any. Personas
    /// are stable lane identities: two personas on the same agent+model are
    /// still TWO lanes (The Skeptic and The Auditor each keep their own
    /// session), while re-running ONE persona reuses its lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Display name, e.g. "Codex (reviewer)" or the persona's name.
    /// User-renameable.
    pub name: String,
    /// Lane color, used by every attributed view.
    pub color: String,
}

/// Per-participant native session state for one thread. `covered_until_seq` is
/// the highest persisted event seq this lane's session has absorbed — either by
/// running the turns itself or via an injected handoff seed. Events past it are
/// what the next seed for this lane must cover (Traycer's coveredUntil).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnchor {
    pub session_id: String,
    #[serde(default)]
    pub covered_until_seq: u64,
    /// Sub-provider that produced this session, when the agent kind has more
    /// than one: the Claudex profile id. A `claude` session id only exists
    /// inside the `CLAUDE_CONFIG_DIR` that created it, so an anchor from a
    /// different profile is unresumable and must be treated as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Edits,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Plan,
    Build,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettings {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub wide_context: bool,
    /// Launch native Claude Code with its `--chrome` integration enabled.
    /// This is deliberately ignored by the Claudex harness and other agents.
    #[serde(default)]
    pub claude_chrome: bool,
    pub access: Access,
    pub mode: Mode,
    /// Signed-in browser profile this chat's browser attaches to. `None` (the
    /// default) means a disposable browser with no stored session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_profile_id: Option<String>,
}

/// Metadata for a chat attachment, persisted in the event log. The bytes live
/// on disk under `~/.threadknot/attachments/<threadId>/<id>.<ext>` and are served
/// (token-gated) so the transcript can re-render thumbnails after a reload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// An attachment as sent on the wire in `turn.start` — carries the raw bytes
/// as a base64 string (no `data:` prefix). Persisted server-side, then dropped.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingAttachment {
    pub name: String,
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: String,
}

/// One root a workspace holds: a project (folder) on a specific machine.
/// `name`/`path` are display snapshots taken when the root was attached so
/// every member machine can label remote roots without a live query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMember {
    pub machine_id: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// The sidebar's top-level element: a renameable container grouping threads
/// from member projects — possibly on different machines — under one name
/// (e.g. "ServiceStorm" holding a Mac root and a dev-machine root). `Project`
/// keeps its exact meaning (one folder on one machine); the workspace sits
/// above it. Replicated to every member machine once peering lands; renames
/// reconcile last-write-wins by `updated_at`. Migration wraps each existing
/// project 1:1, reusing the project id as the workspace id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    /// Small square data URL shown beside the workspace in the sidebar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Whether the user has starred this workspace in the sidebar. Absent/false
    /// are equivalent, so it is skipped when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite: Option<bool>,
    /// Whether the user has stashed this workspace out of the sidebar. Lives on
    /// the workspace rather than on a `Project` because a workspace is the thing
    /// that spans machines: hiding "ServiceStorm" has to take its Mac root and
    /// its dev-machine root with it, and a workspace whose only root is on an
    /// offline peer must still be hideable from here. Replicated whole like
    /// `favorite`, so a project put away on one machine is put away on all of
    /// them. Absent/false are equivalent, so it is skipped when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default)]
    pub members: Vec<WorkspaceMember>,
}

/// A user-crafted appearance theme (Settings → Appearance). A named palette
/// that starts from a preset base + accent, with optional overrides for the
/// neutral slots and an optional background image. Lives server-side because a
/// data-URL background overflows the client's localStorage budget; it is
/// machine-local (never mesh-replicated) and read back via `theme.list`.
/// Modeled on the Hermes registry record — server-owned timestamps, bounded on
/// save. Persisted to `~/.threadknot/themes.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomTheme {
    pub id: String,
    pub name: String,
    /// Preset palette the theme starts from (a THEMES id, e.g. "midnight").
    pub base: String,
    /// Accent preset id or `#rrggbb`.
    pub accent: String,
    /// Optional overrides for the neutral palette slots; keys are CSS var names
    /// WITHOUT the leading `--`, e.g. "bg", "panel", "text".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<std::collections::HashMap<String, String>>,
    /// Optional background image as a data URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_image: Option<String>,
    /// 0..0.9 dim laid over the background image so content stays readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_dim: Option<f64>,
    /// Wallpaper zoom, 1..3 (1 = plain cover). Absent = 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_zoom: Option<f64>,
    /// Horizontal pan, -100..100 (0 = centered); percent of the available
    /// overflow in that axis. Absent = 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_x: Option<f64>,
    /// Vertical pan, -100..100 (0 = centered); percent of the available
    /// overflow in that axis. Absent = 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_y: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

/// A git repository discovered inside a project folder. Projects are FOLDERS
/// that may hold several repos (frontend/, backend/, …); records persist so
/// the id is stable across restarts, keyed by `project_id` + `rel_path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRecord {
    pub id: String,
    pub project_id: String,
    /// Project-relative path to the repo root; `""` = the project root itself.
    pub rel_path: String,
    pub created_at: String,
}

/// A deliverable the agent produced during a turn (a generated PDF, image,
/// CSV, report, …). The primary channel is the agent explicitly registering
/// the file via the `publish_artifact` MCP tool (`origin: "published"`);
/// a conservative turn-diff fallback catches undeclared outputs
/// (`origin: "detected"`, see `artifacts::select_artifacts`). The bytes are
/// snapshot-copied to `~/.threadknot/artifacts/<threadId>/<id>.<ext>` so the
/// artifact stays viewable/downloadable even if the working-tree file is
/// later moved or deleted. Belongs to both a thread (chat card) and a project
/// (the Artifacts tab).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRecord {
    pub id: String,
    pub thread_id: String,
    pub project_id: String,
    /// Basename shown in the UI (or the agent-supplied title when published).
    pub name: String,
    /// Project-relative source path (for "reveal in Files").
    pub rel_path: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Producing agent: "claude" | "codex".
    pub source: String,
    /// "created" the first time we see the path, "modified" on re-write.
    pub op: String,
    /// "published" (agent registered it via the MCP tool) or "detected"
    /// (turn-diff fallback). Records predating the field are detected.
    #[serde(default = "default_artifact_origin")]
    pub origin: String,
    /// Agent-supplied summary of the deliverable (published artifacts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: String,
}

pub fn default_artifact_origin() -> String {
    "detected".to_string()
}

/// Filename to save an artifact under.
///
/// `name` doubles as the display label, and a *published* artifact's label is
/// whatever title the agent passed ("BTC-USD price — Yahoo Finance"), which
/// generally carries no extension. Saving under that name yields a file the OS
/// can't open, so fall back to the real extension from `rel_path` whenever the
/// label doesn't already end in it.
pub fn artifact_file_name(rec: &ArtifactRecord) -> String {
    let ext = std::path::Path::new(&rec.rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let name = rec.name.trim();
    if name.is_empty() {
        return std::path::Path::new(&rec.rel_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rec.id.clone());
    }
    if ext.is_empty() || name.to_ascii_lowercase().ends_with(&format!(".{}", ext.to_ascii_lowercase())) {
        return name.to_string();
    }
    format!("{name}.{ext}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Idle,
    Running,
    WaitingApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub project_id: String,
    /// The machine this thread lives and runs on — REQUIRED and immutable
    /// once set (a thread is pinned to its machine forever). Every machine in
    /// the mesh is stamped explicitly; "" appears only transiently while
    /// reading pre-mesh records, and the startup migration rewrites those.
    #[serde(default)]
    pub machine_id: String,
    /// The agent the NEXT turn runs on. Mutable mid-thread (thread.setAgent);
    /// history stays valid because events are provider-agnostic.
    pub agent: Agent,
    pub title: String,
    pub settings: ThreadSettings,
    /// Legacy mirror of the current agent's session id (kept for old clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_session_id: Option<String>,
    /// Native session per PARTICIPANT, so returning to a lane resumes its own
    /// session and only the delta since `covered_until_seq` needs seeding.
    ///
    /// Keyed by [`Participant::id`]. Anchors written before Parley were keyed by
    /// agent kind, whose JSON key is exactly [`Agent::wire_id`] — the implicit
    /// Builder reuses that id, so old records deserialize and resume untouched.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub session_anchors: std::collections::HashMap<String, SessionAnchor>,
    /// Hermes Runs API id while a remote run is active. Unlike local provider
    /// processes, the gateway run survives a Threadknot restart, so this lets the
    /// new process reattach instead of launching duplicate work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_run_id: Option<String>,
    pub status: ThreadStatus,
    /// When the user parked this thread in the sidebar's settled shelf.
    /// `None` means "not explicitly parked" — the client may still settle it
    /// on its own once it has been quiet long enough. Server-side so the
    /// shelf is the same on the desktop and on a paired phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    /// When the user last pulled this thread back out of the shelf. Without
    /// it a thread auto-settled for being idle would re-settle on the very
    /// next render. It is a TIMESTAMP rather than a flag on purpose: it
    /// restarts the idle clock instead of pinning the thread active forever,
    /// so "un-settle it to read it" doesn't quietly exempt a thread from
    /// auto-settle for good. Cleared by the same new-activity hook that
    /// clears `settled_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_active_at: Option<String>,
    /// Whether the user has starred this thread in the sidebar. Absent/false
    /// are equivalent, so it is skipped when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite: Option<bool>,
    /// The lanes in this thread (Parley). EMPTY is the overwhelmingly common
    /// case and means "one implicit Builder" — see [`Thread::participants_resolved`].
    /// Materialized the moment a second participant joins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<Participant>,
    /// The lane currently mid-turn, or the one that spoke most recently.
    /// `None` on a thread that has never run a turn. Only ever one at a time in
    /// Phase 1 — concurrency is a later phase — so `status` remains a truthful
    /// whole-room aggregate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_speaker: Option<String>,
    /// The multi-agent debate currently running on this thread, if any
    /// (Parley rounds — see `Hub::start_parley`). Persisted so every client
    /// renders the same "round 2 of 3" and the scheduler reads its own state
    /// back at each turn boundary rather than trusting memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parley: Option<ParleyState>,
    pub created_at: String,
    pub updated_at: String,
}

/// A live multi-agent debate (Parley). The scheduler is a deterministic state
/// machine — never an LLM — driven entirely by this struct: at each turn
/// boundary the hub records what the finished turn means (`in_flight`), then
/// seats whoever the schedule says is next.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParleyState {
    /// Reviewer lane ids, in speaking order.
    pub lanes: Vec<String>,
    /// 1-based round being spoken.
    pub round: u32,
    pub max_rounds: u32,
    /// Index into `lanes` of the next reviewer to speak this round;
    /// `== lanes.len()` once every reviewer has spoken (the Builder's slot).
    pub next: usize,
    /// Lanes whose latest turn ended OBJECTING this round. Emptied when the
    /// Builder's answer completes the round.
    #[serde(default)]
    pub objectors: Vec<String>,
    /// Whether any reviewer has ever objected — gates the execution turn
    /// (a debate nobody ever objected in has nothing to implement).
    #[serde(default)]
    pub had_objections: bool,
    /// Run the Builder's implement-what-you-conceded turn on convergence.
    #[serde(default)]
    pub execute: bool,
    /// A message the user typed while the parley held the floor. It always
    /// speaks next — the debate pauses for it and resumes after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_user: Option<String>,
    /// What the turn currently in flight means to the schedule. Written when
    /// the turn is SEATED, read at its boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<ParleyFlight>,
    /// Lane id → personality text, captured at start so every round's brief
    /// carries the same persona voice (see `personas.rs`).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub personalities: std::collections::HashMap<String, String>,
    /// How the parley is wrapping up, once its productive rounds are over —
    /// the closing turn that leaves a deliverable (fixes, the agreed plan, or
    /// the open questions) instead of a dangling status line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<ParleyWrap>,
}

/// What the in-flight parley turn is, so the boundary handler knows how to
/// score it. `User` turns (queued interjections) are scored as neutral — the
/// debate simply resumes afterward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParleyFlight {
    Reviewer,
    Answer,
    Execute,
    /// The closing turn: the agreed plan after convergence, or the open
    /// questions at the round cap.
    Verdict,
    User,
}

/// How a parley is wrapping up. Set once the debate's productive rounds end;
/// the scheduler seats the matching closing turn instead of just stopping,
/// so a parley always leaves a deliverable, never a dangling "so what now?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParleyWrap {
    /// Converged with conceded fixes and `execute` on: implement them.
    Execute,
    /// Converged without execution: write the agreed plan.
    Plan,
    /// Round cap hit: summarize settled vs. open, with the exact questions
    /// the user must answer.
    Escalation,
}

impl Thread {
    /// The thread's lanes, synthesizing the implicit Builder for a thread that
    /// has never had a second participant. Every caller that needs a lane goes
    /// through here so single-participant threads need no migration and behave
    /// byte-for-byte as they did before Parley.
    pub fn participants_resolved(&self) -> Vec<Participant> {
        if !self.participants.is_empty() {
            return self.participants.clone();
        }
        vec![Participant {
            id: self.agent.wire_id().to_string(),
            agent: self.agent,
            settings: self.settings.clone(),
            role: ParticipantRole::Builder,
            persona: None,
            name: self.agent.display_name().to_string(),
            color: default_lane_color(0),
        }]
    }

    /// The lane that owns the work: the first Builder, else the first lane.
    /// This is where the next turn routes once a Reviewer has had its say.
    pub fn primary_participant(&self) -> Participant {
        let all = self.participants_resolved();
        all.iter()
            .find(|p| p.role == ParticipantRole::Builder)
            .cloned()
            .unwrap_or_else(|| all[0].clone())
    }

    pub fn participant(&self, id: &str) -> Option<Participant> {
        self.participants_resolved().into_iter().find(|p| p.id == id)
    }

    /// The lane a turn should be attributed to: the active speaker when it
    /// still exists, else the primary. Never fails, so event attribution and
    /// anchor bookkeeping always have a subject.
    pub fn speaking_participant(&self) -> Participant {
        self.active_speaker
            .as_deref()
            .and_then(|id| self.participant(id))
            .unwrap_or_else(|| self.primary_participant())
    }
}

/// Lane colors, assigned by join order. Deliberately distinguishable at a
/// glance and legible on both themes; the user can override per participant.
pub fn default_lane_color(index: usize) -> String {
    const LANE_COLORS: [&str; 6] = [
        "#5b8def", // builder blue
        "#e0803c", // reviewer amber
        "#3fa06b", // green
        "#a86bd1", // violet
        "#d4566f", // rose
        "#3ea8a8", // teal
    ];
    LANE_COLORS[index % LANE_COLORS.len()].to_string()
}

/// A lightweight summary of a thread's persisted log for the sidebar's hover
/// cards (`thread.preview`). Everything is derived by scanning the event log;
/// an unknown or empty thread answers with `turn_count: 0` and no text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPreview {
    pub thread_id: String,
    /// Last assistant reply in the log, trimmed to about 300 chars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Last user message, trimmed to about 200 chars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_user: Option<String>,
    /// Completed turns in the log (count of turn_completed events).
    pub turn_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    pub tone: String, // "allow" | "deny" | "allowAlways"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One clarifying question the agent asked (AskUserQuestion / requestUserInput).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// Provider answer key: question TEXT for Claude, question id for Codex.
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    /// Whether a free-text "Other" answer is allowed.
    #[serde(default)]
    pub allow_other: bool,
    #[serde(default)]
    pub is_secret: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Context-window tokens currently in use (drives the context meter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_tokens: Option<u64>,
    /// The model's total context-window size, in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Normalized agent activity, identical for every provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentMeta>,
        /// Note submitted while work is active (`turn.steer`). Providers may
        /// inject it immediately or queue it for their next prompt boundary; it
        /// must not re-baseline artifacts, flip thread status, or start a turn.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mid_turn: bool,
        /// Machine-issued instruction rather than something the human typed —
        /// a Parley role prompt handing a lane its brief. It occupies the user
        /// slot because that is the only way to address a provider session, but
        /// the UI renders it as a divider and the transcript labels it as an
        /// instruction, so later lanes never mistake it for the user talking.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        injected: bool,
    },
    /// Provenance (which provider/model ran this turn) is persisted so history
    /// renders correctly after mid-thread agent switches.
    TurnStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    AssistantDelta {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    Thinking {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolStart {
        call_id: String,
        name: String,
        detail: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolOutputDelta {
        call_id: String,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolEnd {
        call_id: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        /// Set only on a thread replay, when `output` has had its middle
        /// elided to keep the log small. The full text stays on disk and the
        /// client pulls it with `thread.toolOutput` if the card is expanded.
        /// Never persisted and never set on live events.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    FileDiff {
        path: String,
        unified: String,
    },
    /// A deliverable the agent produced this turn (see [`ArtifactRecord`]).
    /// Emitted just before the turn boundary so it renders as a card in the
    /// chat that produced it; the durable bytes are fetched via `/artifact-file`.
    #[serde(rename_all = "camelCase")]
    Artifact {
        id: String,
        name: String,
        rel_path: String,
        mime_type: String,
        size_bytes: u64,
        op: String,
        /// "published" | "detected" (see [`ArtifactRecord::origin`]).
        #[serde(default = "default_artifact_origin")]
        origin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ApprovalRequest {
        approval_id: String,
        approval_kind: String, // "tool" | "exec" | "patch" | "plan"
        title: String,
        detail: String,
        options: Vec<ApprovalOption>,
    },
    #[serde(rename_all = "camelCase")]
    ApprovalResolved {
        approval_id: String,
        option_id: String,
    },
    #[serde(rename_all = "camelCase")]
    QuestionRequest {
        request_id: String,
        questions: Vec<Question>,
    },
    #[serde(rename_all = "camelCase")]
    QuestionResolved {
        request_id: String,
        /// Persisted so handoff seeds can carry what the user actually answered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<std::collections::HashMap<String, Vec<String>>>,
    },
    /// Latest provider-reported context-window occupancy. This is independent
    /// of a turn boundary so streaming usage, compaction, and model changes can
    /// refresh the composer meter immediately.
    ContextUsage {
        usage: Usage,
    },
    TurnCompleted {
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    TurnAborted,
    #[serde(rename_all = "camelCase")]
    SessionStarted {
        provider_session_id: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<Agent>,
    },
    Status {
        text: String,
    },
    /// A subagent (Claude `Task`/background `Agent` tool) began. `toolUseId`
    /// links it to the parent's [`ToolStart`] card so the UI can nest it;
    /// `background` distinguishes fire-and-forget agents (whose completion later
    /// wakes the main turn) from synchronous ones that stream inline.
    #[serde(rename_all = "camelCase")]
    SubagentStarted {
        task_id: String,
        tool_use_id: String,
        description: String,
        subagent_type: String,
        background: bool,
        /// The brief delegated to the child, when the provider exposes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Live activity from a synchronous subagent (its own assistant text /
    /// tool use), attributed to `taskId`. Transient — a running summary line,
    /// not persisted history.
    #[serde(rename_all = "camelCase")]
    SubagentProgress {
        task_id: String,
        /// "thinking" | "text" | "tool"
        activity: String,
        text: String,
    },
    /// A subagent finished. `summary` is the model's one-line result (from the
    /// `task_notification`); `status` is "completed" / "error" / etc.
    #[serde(rename_all = "camelCase")]
    SubagentCompleted {
        task_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    Error {
        message: String,
    },
}

impl AgentEvent {
    /// Deltas are streamed to clients but never persisted.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            AgentEvent::AssistantDelta { .. }
                | AgentEvent::ThinkingDelta { .. }
                | AgentEvent::ToolOutputDelta { .. }
                | AgentEvent::SubagentProgress { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedEvent {
    pub seq: u64,
    pub ts: String,
    /// Which lane produced this event ([`Participant::id`]), or `None` for the
    /// user and for anything the server itself emitted. Deliberately on the
    /// ENVELOPE rather than each `AgentEvent` variant: one field attributes
    /// every event kind — messages, tool calls, thinking, diffs, artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub event: AgentEvent,
}

/// Human-readable copy for an attention-worthy event. This lives on the live
/// event envelope rather than in [`AgentEvent`], so notification presentation
/// is not duplicated into the durable transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventNotice {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    /// Optional profile picture for model-backed agent profiles (Hermes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_wide_context: Option<bool>,
    /// A model whose context window is fixed rather than user-selectable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_context_window: Option<u64>,
    /// Reasoning-effort options this model supports (None = no effort control).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub efforts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub id: Agent,
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_hint: Option<String>,
    pub models: Vec<ModelInfo>,
    pub default_model: String,
}

/// One provider rate-limit window (5-hour session, weekly, …), normalized
/// across providers the way Traycer does it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateWindow {
    /// Short display label: "5h", "Week", "3d", …
    pub label: String,
    pub used_percent: f64,
    /// ISO timestamp when this window resets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_mins: Option<u64>,
}

/// Subscription usage snapshot for one provider (drives the sidebar meter).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsage {
    pub agent: Agent,
    pub available: bool,
    /// Plan label ("Max", "Pro", …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<RateWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub fetched_at: String,
}

/// Live presence for one registered Hermes gateway (the sidebar Online/Offline
/// dot). Produced by the background health poller in `hermes.rs`; keyed by the
/// registry agent id.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesAgentStatus {
    pub agent_id: String,
    pub online: bool,
    /// ISO timestamp of the most recent probe. Truthful, but bumps every poll,
    /// so it is NOT a "live freshness" signal for connected clients.
    pub last_checked_at: String,
    /// ISO timestamp the CURRENT online/offline state was first entered
    /// (preserved across polls that do not flip it). Drives "offline since …".
    pub since_at: String,
    /// Probe round-trip in milliseconds (present only when online).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Gateway-advertised version string (present only when online and known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A full presence readout for the registered Hermes gateways, tagged with a
/// monotonically increasing per-process `revision`. Both the
/// `hermes.agent.statuses` snapshot and the `hermes.statuses` broadcast carry
/// this shape so the client can drop a stale frame that loses a delivery race
/// with a fresher one (see `HermesStatusState`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HermesStatusSnapshot {
    pub revision: u64,
    pub statuses: Vec<HermesAgentStatus>,
}

/// When a scheduled run recurs. Modeled on Codex's ScheduledTaskSchedule:
/// human presets, no cron. Times are LOCAL "HH:MM"; days use 0=Sunday..6=Saturday.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Cadence {
    /// Fires at minute 0 of every local hour divisible by `every_hours`.
    #[serde(rename_all = "camelCase")]
    Hourly {
        every_hours: u32,
    },
    Daily {
        time: String,
    },
    Weekdays {
        time: String,
    },
    Weekly {
        days: Vec<u8>,
        time: String,
    },
}

/// A recurring agent run: each firing creates a fresh thread in the project
/// and starts a turn with `prompt`, so results live in the normal thread list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub project_id: String,
    pub agent: Agent,
    pub settings: ThreadSettings,
    pub name: String,
    pub prompt: String,
    pub cadence: Cadence,
    pub enabled: bool,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Next planned firing (ISO, local-derived); recomputed on edit and after
    /// each firing, and lazily by the scheduler when missing or stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
}

/// A project-bound terminal tab. The record persists (so tabs survive an app
/// restart and can be renamed); the live pty session is tracked separately in
/// `term::TermRegistry` and keyed by `(project_id, id)`. Its scrollback is
/// flushed to `~/.threadknot/terminals/<id>.scrollback` and replayed on reopen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRecord {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub created_at: String,
}

/// A terminal captured inside an archive: metadata plus its persisted scrollback
/// (UTF-8-lossy string, may be empty) so the transcript can be restored offline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedTerminal {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub scrollback: String,
}

/// Lightweight archive descriptor returned by list/archive/restore RPCs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveHeader {
    pub id: String,
    pub thread_id: String,
    pub title: String,
    pub agent: Agent,
    pub project_id: String,
    pub project_name: String,
    pub archived_at: String,
    pub updated_at: String,
    pub event_count: u64,
    pub terminal_count: u64,
}

/// Self-contained archive file persisted at `<archiveDir>/<id>.json`. The header
/// fields are flattened to the top level so a single file carries everything
/// needed to restore the thread without touching the live store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveFile {
    #[serde(flatten)]
    pub header: ArchiveHeader,
    pub project_path: String,
    pub thread: Thread,
    pub events: Vec<PersistedEvent>,
    pub terminals: Vec<ArchivedTerminal>,
}

/// Client -> server request envelope.
#[derive(Debug, Deserialize)]
pub struct ClientRequest {
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Whose authority this request carries, when it arrived over a peer link.
    ///
    /// One peer socket multiplexes requests from every client on that machine —
    /// its owner, and each of its paired phones — so the assertion has to be
    /// per-request rather than per-connection. It is a sibling of `payload`, not
    /// a field inside it, so it can never collide with a real request parameter
    /// or be smuggled in by a client that controls a payload.
    ///
    /// **Only honoured on a connection that authenticated as a peer.** A phone
    /// or a LAN browser can put this in a frame all it likes; it is discarded.
    #[serde(default)]
    pub mesh: Option<MeshAssertion>,
}

/// The `mesh` sibling of a routed request frame.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshAssertion {
    /// Capability names the originating caller held. `None` means the request
    /// came from the sending machine's own owner. Names are parsed leniently:
    /// an unrecognised one is dropped, never honoured.
    #[serde(default)]
    pub on_behalf_of: Option<Vec<String>>,
}

/// Server -> client frames.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "response")]
    Response {
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "event", rename_all = "camelCase")]
    Event {
        thread_id: String,
        seq: i64,
        ts: String,
        /// Producing lane; see [`PersistedEvent::speaker`].
        #[serde(skip_serializing_if = "Option::is_none")]
        speaker: Option<String>,
        event: AgentEvent,
        /// Precomposed by the thread's owning server so desktop, browser and
        /// phone pushes all describe the event consistently. Optional for wire
        /// compatibility with older peers.
        #[serde(skip_serializing_if = "Option::is_none")]
        notice: Option<EventNotice>,
    },
    #[serde(rename = "state.changed", rename_all = "camelCase")]
    StateChanged {
        scope: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
    },
    /// Pushed whenever a provider usage snapshot refreshes.
    #[serde(rename = "usage")]
    Usage { usage: Vec<ProviderUsage> },
    /// Pushed when a Hermes gateway's live presence changes (an agent flipping
    /// online<->offline, or being added/removed). Carries the full fresh
    /// snapshot plus its `revision`; `hermes.agent.statuses` returns the same
    /// shape on request so the client can order the two against each other.
    #[serde(rename = "hermes.statuses", rename_all = "camelCase")]
    HermesStatuses {
        revision: u64,
        statuses: Vec<HermesAgentStatus>,
    },
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The same wire format as [`now_iso`], for a deadline computed in the future.
///
/// Used for a countdown the browser renders: the client subtracts its own clock,
/// so the value has to be an absolute instant rather than a duration that goes
/// stale between the response being built and the page painting it.
pub fn iso_from(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, rel_path: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: "id".into(),
            thread_id: "t".into(),
            project_id: "p".into(),
            name: name.into(),
            rel_path: rel_path.into(),
            mime_type: "image/png".into(),
            size_bytes: 1,
            source: "claude".into(),
            op: "created".into(),
            origin: "published".into(),
            description: None,
            created_at: "now".into(),
        }
    }

    #[test]
    fn published_title_gains_the_real_extension() {
        // The regression: an agent-supplied title carries no extension, so the
        // download landed as an extensionless file the OS could not open.
        assert_eq!(
            artifact_file_name(&rec("BTC-USD price — Yahoo Finance", "shots/screenshot-1.png")),
            "BTC-USD price — Yahoo Finance.png"
        );
    }

    #[test]
    fn a_title_that_already_ends_in_the_extension_is_left_alone() {
        assert_eq!(artifact_file_name(&rec("report.pdf", "out/report.pdf")), "report.pdf");
        assert_eq!(artifact_file_name(&rec("REPORT.PDF", "out/report.pdf")), "REPORT.PDF");
    }

    #[test]
    fn a_dotted_title_still_gets_the_extension() {
        // "v1.2" must not be mistaken for an extension of "2".
        assert_eq!(
            artifact_file_name(&rec("Q3 brief v1.2", "out/brief.pdf")),
            "Q3 brief v1.2.pdf"
        );
    }

    #[test]
    fn falls_back_to_the_basename_when_there_is_no_title() {
        assert_eq!(artifact_file_name(&rec("  ", "shots/screenshot-1.png")), "screenshot-1.png");
    }

    #[test]
    fn extensionless_source_keeps_the_title_as_is() {
        assert_eq!(artifact_file_name(&rec("Makefile copy", "Makefile")), "Makefile copy");
    }
}
