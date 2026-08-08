// Threadknot client <-> server protocol types (mirrors docs/PROTOCOL.md v1)

/** `claudex` is the Claude Code harness driven by a non-Anthropic model over
 *  an Anthropic-compatible bridge; its "models" are Claudex profiles. */
export type Agent = "claude" | "codex" | "kimi" | "hermes" | "claudex";

/** Display names for the agent kinds — mirrors `Agent::display_name`. */
export const AGENT_LABELS: Record<Agent, string> = {
  claude: "Claude Code",
  codex: "Codex",
  kimi: "Kimi Code",
  hermes: "Hermes",
  claudex: "Claudex",
};

/** Reserved server-side project hosting Hermes-agent threads (they have no
 *  folder). Never wrapped in a workspace — the sidebar renders its threads in
 *  the dedicated Hermes section, and folder-based UI must skip it. */
export const HERMES_HOME_PROJECT_ID = "hermes-home";

export interface Project {
  id: string;
  name: string;
  path: string;
  createdAt: string;
}

/** One root a workspace holds: a project (folder) on a specific machine.
 *  `name`/`path` are display snapshots from attach time so remote roots can
 *  be labeled without a live query. */
export interface WorkspaceMember {
  machineId: string;
  projectId: string;
  name?: string;
  path?: string;
}

/** The sidebar's top-level element: a renameable container grouping threads
 *  from member projects — possibly on different machines — under one name
 *  (e.g. "ServiceStorm"). Projects keep their meaning (one folder on one
 *  machine); migration wraps each existing project 1:1, reusing the project
 *  id as the workspace id. */
export interface Workspace {
  id: string;
  name: string;
  image?: string;
  createdAt: string;
  updatedAt: string;
  /** Whether the user has starred this workspace in the sidebar; absent = false. */
  favorite?: boolean;
  /** Whether the user has stashed this workspace out of the sidebar; absent =
   *  false. Set on the workspace, not on a project, so it takes every root with
   *  it — including roots on machines that are offline right now — and it rides
   *  the mesh replica, so a project put away here is put away everywhere. */
  hidden?: boolean;
  members: WorkspaceMember[];
}

/** A paired peer Threadknot machine (Settings → machines). Its master token is
 *  held server-side only and never serialized to clients. */
export interface PeerInfo {
  machineId: string;
  name: string;
  port: number;
  /** Candidate addresses, best first — hints, not identity. */
  addresses: string[];
  lastGoodAddress?: string;
  lastSeenAt?: string;
  addedAt: string;
  meshVersion: number;
  /** Present in peer.list responses (live presence). */
  online?: boolean;
  /** Paired before Threadknot encrypted mesh connections (SEC-012). Such a pair
   *  is refused rather than downgraded to the old plaintext transport, so it is
   *  never `online` — and saying only "offline" would send someone looking for
   *  a network fault instead of updating the other machine and re-pairing. */
  needsUpgrade?: boolean;
  /** Appearance the peer advertises for itself (data URL / accent color). */
  avatar?: string;
  color?: string;
  /** Local overrides set here via peer.setAppearance: they win over the
   *  peer-advertised look. */
  avatarOverride?: string;
  colorOverride?: string;
}

/** An unpaired Threadknot seen via mDNS on the local network. */
export interface DiscoveredPeer {
  machineId: string;
  name: string;
  addresses: string[];
  port: number;
  meshVersion: number;
}

/** Identity + live capabilities of a Threadknot install (device.info). */
export interface DeviceInfo {
  machineId: string;
  friendlyName: string;
  hostname: string;
  os: string;
  arch: string;
  version: string;
  meshVersion: number;
  capabilities: string[];
  /** This machine's own appearance (device.setAppearance). */
  avatar?: string;
  color?: string;
  /** ISO-8601 clock for peer-to-peer last-write-wins profile gossip. */
  profileUpdatedAt?: string;
}

export type Access = "read" | "edits" | "full";
export type Mode = "plan" | "build";

export interface ThreadSettings {
  model: string;
  effort?: string; // model-specific; see AgentModel.efforts
  wideContext?: boolean; // claude only: 1M context window
  /** Native Claude only: launch the Claude Code process with --chrome. */
  claudeChrome?: boolean;
  access: Access;
  mode: Mode;
  /**
   * Signed-in browser profile this chat's browser uses. Absent means a
   * disposable browser: no stored logins, no site restrictions.
   */
  browserProfileId?: string | null;
}

/**
 * A durable, signed-in browser. Threadknot stores the SESSION (cookies and site
 * storage the user established by signing in themselves) — never passwords.
 * `origins` is enforced inside the browser: a signed-in profile cannot load a
 * document from anywhere else.
 */
export interface BrowserProfileInfo {
  id: string;
  name: string;
  origins: string[];
  machineId: string;
  createdAt: string;
  lastUsedAt?: string;
}

/* ---------------------------------------------------------------------------
 * The Library: skills and MCP servers (see src-tauri/src/library.rs)
 * ------------------------------------------------------------------------- */

/** Agents with a skills directory Threadknot manages. */
export type SkillTarget = "claude" | "codex" | "kimi";

export const SKILL_TARGETS: { id: SkillTarget; label: string }[] = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "kimi", label: "Kimi Code" },
];

/**
 * Where a skill came from. `plugin` entries are read-only here — a Claude Code
 * plugin owns their lifecycle, so Threadknot shows them but will not delete them.
 */
export type SkillOrigin =
  | { kind: "library" }
  | { kind: "local" }
  | { kind: "plugin"; plugin: string };

/** One skill folder, folded across every agent that has it installed. */
export interface InstalledSkill {
  /** Directory name — the id removal takes. */
  id: string;
  title: string;
  description: string;
  targets: SkillTarget[];
  origin: SkillOrigin;
  catalogId?: string;
  /** `owner/repo/path@ref` when Threadknot installed it. */
  source?: string;
  installedAt?: string;
  files: number;
  bytes: number;
  path: string;
  removable: boolean;
  /** A Claude Code plugin also ships this skill name. Removing the copy Threadknot
   *  manages will not stop Claude finding the plugin's, so the card says so. */
  alsoFromPlugin?: string;
}

export type McpTransport =
  | { type: "stdio"; command: string; args: string[]; env: Record<string, string> }
  | { type: "http"; url: string; headers: Record<string, string> };

/**
 * A user-installed MCP server. Unlike skills, nothing on disk discovers these:
 * Threadknot injects every enabled entry into each driver at spawn.
 */
export interface McpServerInfo {
  id: string;
  /** Tool namespace the agent sees (`mcp__<name>__<tool>`). */
  name: string;
  label: string;
  description: string;
  transport: McpTransport;
  enabled: boolean;
  /** Empty = every agent Threadknot can inject into. */
  agents: Agent[];
  catalogId?: string;
  createdAt: string;
}

/** Where a catalog skill's files come from. */
export type SkillSourceRef =
  /** Fetched from a public GitHub repo at install time. */
  | { source: "github"; repo: string; gitRef: string; path: string }
  /** Written by Threadknot and embedded in the binary — installs offline. */
  | { source: "bundled" };

export type CatalogSkill = {
  id: string;
  title: string;
  description: string;
  category: string;
  license: string;
  homepage: string;
} & SkillSourceRef;

/** A value the user supplies when installing a catalog MCP server. */
export interface CatalogInput {
  key: string;
  label: string;
  placeholder: string;
  help: string;
  secret: boolean;
  optional: boolean;
}

export interface CatalogMcp {
  id: string;
  name: string;
  title: string;
  description: string;
  category: string;
  transport: McpTransport;
  inputs: CatalogInput[];
  /** Binary this server needs on PATH, if any. */
  prereq?: string;
  /** Whether that binary is actually present on the machine being managed. */
  prereqOk: boolean;
  homepage: string;
}

export interface LibraryCatalog {
  skills: CatalogSkill[];
  mcp: CatalogMcp[];
}

export interface LibraryData {
  skills: InstalledSkill[];
  mcpServers: McpServerInfo[];
  catalog: LibraryCatalog;
}

export type ThreadStatus = "idle" | "running" | "waiting_approval";

export interface SessionAnchor {
  sessionId: string;
  coveredUntilSeq: number;
}

export type ParticipantRole = "builder" | "reviewer";

/**
 * One lane in a thread (Parley): a provider session with its own identity,
 * model and access level, anchored into the thread's single shared event log.
 * Two lanes may run the same agent and model — they're told apart by `id`,
 * which is also what `PersistedEvent.speaker` carries.
 */
export interface Participant {
  id: string;
  agent: Agent;
  settings: ThreadSettings;
  role: ParticipantRole;
  name: string;
  /** Lane color, used by every attributed view. */
  color: string;
}

export interface Thread {
  id: string;
  projectId: string;
  /** The machine this thread lives and runs on — immutable once set. Absent
   *  only on servers older than the mesh migration. */
  machineId?: string;
  /** The agent the NEXT turn runs on — switchable mid-thread (thread.setAgent). */
  agent: Agent;
  title: string;
  settings: ThreadSettings;
  providerSessionId?: string;
  /** Per-PARTICIPANT native session + how much of the log it has absorbed,
   *  keyed by `Participant.id`. (Threads written before Parley key these by
   *  agent name, which is exactly the implicit builder's id.) */
  sessionAnchors?: Record<string, SessionAnchor>;
  /** Active remote Hermes run, persisted so Threadknot can reattach after restart. */
  providerRunId?: string;
  status: ThreadStatus;
  /** When the user parked this chat in the settled shelf. Absent = not
   *  explicitly settled (it may still settle itself once it goes quiet —
   *  see `threadSettled`). Server-backed, so parking on your phone parks it
   *  on the desktop too. */
  settledAt?: string;
  /** When the user last pulled this chat back out of the shelf. A timestamp
   *  rather than a flag so it RESTARTS the idle clock instead of pinning the
   *  chat active forever — un-settling something just to read it shouldn't
   *  exempt it from auto-settle for good. Cleared on real activity. */
  keptActiveAt?: string;
  /** Whether the user has starred this thread in the sidebar; absent = false. */
  favorite?: boolean;
  /** The lanes in this thread. Absent/empty is the common case and means one
   *  implicit builder derived from `agent` — materialized when a reviewer joins.
   *  Use `threadParticipants()` rather than reading this directly. */
  participants?: Participant[];
  /** The lane currently mid-turn, or the one that spoke most recently. */
  activeSpeaker?: string;
  /** The multi-agent debate currently running on this thread, if any. */
  parley?: ParleyState;
  createdAt: string;
  updatedAt: string;
}

export type ParleyFlight = "reviewer" | "answer" | "execute" | "verdict" | "user";

/**
 * A named, reusable reviewer preset (Parley persona): the agent that powers
 * it plus a personality folded into every review brief it runs. Built-ins are
 * seeded server-side on first run; all are editable. Lives in personas.json
 * on the machine; edited via persona.save/delete.
 */
export interface ReviewerPersona {
  id: string;
  name: string;
  agent: Agent;
  /** Absent = the agent's current default model at seat time. */
  model?: string;
  effort?: string;
  /** Absent = the default (full control). */
  access?: Access;
  /** Folded into every review brief this persona runs. */
  personality: string;
  builtin: boolean;
  createdAt: string;
}

/**
 * A live multi-agent debate (Parley rounds). The server-side scheduler is a
 * deterministic state machine driven by this struct: at each turn boundary it
 * scores the finished turn (`inFlight`) and seats the next speaker.
 */
export interface ParleyState {
  /** Reviewer lane ids, in speaking order. */
  lanes: string[];
  /** 1-based round being spoken. */
  round: number;
  maxRounds: number;
  /** Index into `lanes` of the next reviewer; == lanes.length once all spoke. */
  next: number;
  /** Lanes whose latest turn this round ended OBJECTING. */
  objectors: string[];
  /** Any objection ever raised — gates the execution turn. */
  hadObjections: boolean;
  /** Run the builder's implement-what-you-conceded turn on convergence. */
  execute: boolean;
  /** A user message queued mid-parley; it speaks next. */
  pendingUser?: string;
  /** What the in-flight turn means to the schedule. */
  inFlight?: ParleyFlight;
  /** Lane id → personality text, so every round's brief keeps the persona. */
  personalities?: Record<string, string>;
  /** How the parley is wrapping up: the closing turn (fixes / agreed plan /
   *  open questions) that always leaves a deliverable. */
  wrap?: "execute" | "plan" | "escalation";
}

/** Lane colors by join order — mirrors `default_lane_color` in the backend. */
const LANE_COLORS = ["#5b8def", "#e0803c", "#3fa06b", "#a86bd1", "#d4566f", "#3ea8a8"];

/**
 * A thread's lanes, synthesizing the implicit builder for a thread that never
 * gained a second participant — the client-side mirror of
 * `Thread::participants_resolved`. Every attributed view goes through here so
 * ordinary single-agent threads render identically to before.
 */
export function threadParticipants(thread: Thread): Participant[] {
  if (thread.participants?.length) return thread.participants;
  return [
    {
      id: thread.agent,
      agent: thread.agent,
      settings: thread.settings,
      role: "builder",
      name: AGENT_LABELS[thread.agent] ?? thread.agent,
      color: LANE_COLORS[0],
    },
  ];
}

/** Lane lookup by id, for attributing an event to its speaker. */
export function threadParticipant(
  thread: Thread | undefined,
  id: string | undefined,
): Participant | undefined {
  if (!thread || !id) return undefined;
  return threadParticipants(thread).find((p) => p.id === id);
}

/** A hover-card summary of a thread's persisted log (`thread.preview`).
 *  Derived by scanning the event log; an unknown or empty thread yields
 *  `turnCount: 0` with no text. */
export interface ThreadPreview {
  threadId: string;
  /** Last assistant reply in the log, trimmed to about 300 chars. */
  summary?: string;
  /** Last user message, trimmed to about 200 chars. */
  lastUser?: string;
  /** Completed turns in the log (count of turn_completed events). */
  turnCount: number;
}

/** Header record for an archived (exported + removed) thread. */
export interface ArchiveHeader {
  id: string;
  threadId: string;
  title: string;
  agent: Agent;
  projectId: string;
  projectName: string;
  archivedAt: string;
  updatedAt: string;
  eventCount: number;
  terminalCount: number;
}

export interface AgentModel {
  id: string;
  name: string;
  /** Profile picture for a model-backed agent profile (Hermes). */
  image?: string;
  supportsWideContext?: boolean;
  /** Context size when the model has one fixed, non-selectable window. */
  fixedContextWindow?: number;
  /** Reasoning-effort levels this model supports (e.g. ["low","medium","high","xhigh"]). Absent → no effort control. */
  efforts?: string[];
  defaultEffort?: string;
}

export interface AgentInfo {
  id: Agent;
  name: string;
  available: boolean;
  authHint?: string;
  models: AgentModel[];
  defaultModel: string;
}

export interface HelloData {
  /** Git-derived build version ("0.1.<commit count>"); bumps every commit. */
  version: string;
  /** Short git hash of the commit this build came from; absent on old servers. */
  gitHash?: string;
  /** Commit date (YYYY-MM-DD) of this build; absent on old servers. */
  buildDate?: string;
  /** Carries `?token=` ONLY for the desktop's own master connection; a paired
   *  device is given the origin alone (SEC-001). */
  lanUrl: string;
  /** Whether this connection is the desktop owner or a paired device. */
  principal?: "master" | "device" | "peer";
  /** What this connection may do. Advisory — enforced server-side regardless. */
  capabilities?: DeviceCapability[];
  agents: AgentInfo[];
  /** Named reviewer presets (Parley personas); absent on old servers. */
  personas?: ReviewerPersona[];
  /** Stable install identity (mobile push routing); absent on old servers. */
  serverId?: string;
  serverName?: string;
  /** Mesh identity (== serverId today; separate on the wire so clients never
   *  rely on that). Absent on pre-mesh servers. */
  machineId?: string;
  friendlyName?: string;
  meshVersion?: number;
  /** This machine's own appearance (device.setAppearance). */
  avatar?: string;
  color?: string;
  /** ISO-8601 clock for peer-to-peer last-write-wins profile gossip. */
  profileUpdatedAt?: string;
  /** Whether the composer's mic button can record here; absent on old servers. */
  dictation?: DictationCapability;
}

/** Why voice dictation can or can't run against this server. */
export interface DictationCapability {
  available: boolean;
  /** Explains unavailability in Voice settings; the composer hides the button. */
  hint?: string;
}

export type DictationProvider = "local" | "api";

/** Secret-free server settings. The stored API key is exposed only as a bool. */
export interface DictationSettings {
  provider: DictationProvider;
  baseUrl: string;
  model: string;
  hasApiKey: boolean;
  captureAvailable: boolean;
  captureHint?: string;
  localAvailable: boolean;
  localHint?: string;
}

export interface DictationSettingsInput {
  provider: DictationProvider;
  baseUrl: string;
  model: string;
  /** Absent preserves the stored key; present replaces it. */
  apiKey?: string;
}

/** One commit in the build's embedded changelog (app.changelog). */
export interface ChangelogEntry {
  /** The version the app reported while this commit was HEAD ("0.1.42"). */
  version: string;
  hash: string;
  /** Commit date, YYYY-MM-DD. */
  date: string;
  /** One-line summary of the change. */
  subject: string;
  /** Longer breakdown of what was fixed or done; may be empty. */
  body: string;
}

/** One client-facing release in the build's embedded update notes
 *  (CHANGELOG.md → app.changelog). */
export interface ReleaseNote {
  /** Version label from the changelog header ("0.1.45"); may be empty. */
  version: string;
  /** Date the update went live, YYYY-MM-DD. */
  date: string;
  /** Plain-language bullets: what changed and where to find it. */
  notes: string[];
}

export type ApprovalTone = "allow" | "deny" | "allowAlways";
export type ApprovalKind = "tool" | "exec" | "patch" | "plan";

export interface ApprovalOption {
  id: string;
  label: string;
  tone: ApprovalTone;
}

export interface QuestionOption {
  label: string;
  description: string;
}

export interface Question {
  id: string;
  header: string;
  question: string;
  options: QuestionOption[];
  multiSelect: boolean;
  allowOther: boolean;
  isSecret: boolean;
}

export interface TurnUsage {
  inputTokens?: number;
  outputTokens?: number;
  /** Context-window tokens in use (drives the context meter). */
  usedTokens?: number;
  /** The model's total context-window size, in tokens. */
  maxTokens?: number;
  contextPct?: number;
  costUsd?: number;
}

/** One provider rate-limit window (5-hour session, weekly, …). */
export interface RateWindow {
  label: string;
  usedPercent: number;
  resetsAt?: string;
  windowMins?: number;
}

/** Subscription usage snapshot for one provider (drives the sidebar meter). */
export interface ProviderUsage {
  agent: Agent;
  available: boolean;
  plan?: string;
  windows?: RateWindow[];
  error?: string;
  fetchedAt: string;
}

/** Attachment sent on the wire in `turn.start` — base64 bytes, no data: prefix. */
export interface OutgoingAttachment {
  name: string;
  mimeType: string;
  data: string;
}

/** Attachment metadata persisted in the event log (bytes served separately). */
export interface AttachmentMeta {
  id: string;
  name: string;
  mimeType: string;
  sizeBytes: number;
}

/** A deliverable the agent produced (generated PDF/image/CSV/report/…). Belongs
 *  to a thread (chat card) and a project (Artifacts tab); durable bytes served
 *  via `/artifact-file` (see artifactFileUrl). */
export interface ArtifactRecord {
  id: string;
  threadId: string;
  projectId: string;
  name: string;
  /** Project-relative source path. */
  relPath: string;
  mimeType: string;
  sizeBytes: number;
  /** Producing agent id, e.g. "claude", "codex", or "kimi". */
  source: string;
  /** "created" | "modified". */
  op: string;
  /** "published" (agent registered it via the publish_artifact MCP tool) or
   *  "detected" (turn-diff fallback). Absent on old records = detected. */
  origin?: string;
  /** Agent-supplied summary (published artifacts only). */
  description?: string;
  createdAt: string;
}

export type AgentEvent =
  | {
      kind: "user_message";
      text: string;
      attachments?: AttachmentMeta[];
      /** Submitted while work is active; a provider may inject or queue it. */
      mid_turn?: boolean;
      /** Machine-issued brief (a Parley role prompt) rather than something the
       *  human typed — rendered as a divider, not a user bubble. */
      injected?: boolean;
    }
  | { kind: "turn_started"; agent?: Agent; model?: string }
  | { kind: "assistant_delta"; text: string }
  | { kind: "assistant_message"; text: string }
  | { kind: "thinking_delta"; text: string }
  | { kind: "thinking"; text: string }
  | { kind: "tool_start"; callId: string; name: string; detail: string }
  | { kind: "tool_output_delta"; callId: string; text: string }
  | {
      kind: "tool_end";
      callId: string;
      name: string;
      output?: string;
      isError?: boolean;
      /** Replay only: `output` has its middle elided; `thread.toolOutput` has the rest. */
      truncated?: boolean;
    }
  | { kind: "file_diff"; path: string; unified: string }
  | {
      kind: "artifact";
      id: string;
      name: string;
      relPath: string;
      mimeType: string;
      sizeBytes: number;
      op: string;
      origin?: string;
      description?: string;
    }
  | {
      kind: "approval_request";
      approvalId: string;
      approvalKind: ApprovalKind;
      title: string;
      detail: string;
      options: ApprovalOption[];
    }
  | { kind: "approval_resolved"; approvalId: string; optionId: string }
  | { kind: "question_request"; requestId: string; questions: Question[] }
  | { kind: "question_resolved"; requestId: string; answers?: Record<string, string[]> }
  | { kind: "context_usage"; usage: TurnUsage }
  | { kind: "turn_completed"; usage?: TurnUsage }
  | { kind: "turn_aborted" }
  | { kind: "session_started"; providerSessionId: string; model: string; agent?: Agent }
  | { kind: "status"; text: string }
  | {
      kind: "subagent_started";
      taskId: string;
      toolUseId: string;
      description: string;
      subagentType: string;
      background: boolean;
      prompt?: string;
    }
  | { kind: "subagent_progress"; taskId: string; activity: string; text: string }
  | { kind: "subagent_completed"; taskId: string; status: string; summary?: string }
  | { kind: "error"; message: string };

export interface PersistedEvent {
  seq: number;
  ts: string;
  /** Which lane produced this event (`Participant.id`); absent for the user's
   *  own messages and for anything the server emitted. */
  speaker?: string;
  event: AgentEvent;
}

export interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
}

/** When a scheduled run recurs — human presets, no cron.
 *  Times are LOCAL "HH:MM"; days use 0=Sunday..6=Saturday. */
export type Cadence =
  | { type: "hourly"; everyHours: number }
  | { type: "daily"; time: string }
  | { type: "weekdays"; time: string }
  | { type: "weekly"; days: number[]; time: string };

/** A recurring agent run: each firing creates a fresh thread in the project. */
export interface Schedule {
  id: string;
  projectId: string;
  agent: Agent;
  settings: ThreadSettings;
  name: string;
  prompt: string;
  cadence: Cadence;
  enabled: boolean;
  createdAt: string;
  lastRunAt?: string;
  lastThreadId?: string;
  lastError?: string;
  nextRunAt?: string;
}

/** One entry in the recursive project tree (fs.tree). Paths are
 *  project-root-relative, POSIX-separated, no trailing slash. */
export interface TreeEntry {
  path: string;
  kind: "file" | "dir";
  sizeBytes?: number;
}

export interface FileTreeData {
  root: string;
  entries: TreeEntry[];
  truncated: boolean;
}

export interface FileReadData {
  path: string;
  /** UTF-8 text contents; empty when `binary`. */
  contents: string;
  /** Full on-disk size (may exceed what `contents` holds). */
  byteLength: number;
  truncated: boolean;
  /** NUL-byte heuristic said this isn't text — fetch via /file instead. */
  binary: boolean;
}

/** One git repository inside a project folder (projects may hold several —
 *  frontend/, backend/, mobile/ — or be a single repo at relPath ""). The
 *  record fields (id/relPath) are persisted; the rest is live git state.
 *  `error` set → the repo exists but `git status` failed (fields absent). */
export interface GitRepoInfo {
  id: string;
  projectId: string;
  relPath: string;
  name: string;
  branch?: string;
  detached?: boolean;
  upstream?: string | null;
  ahead?: number;
  behind?: number;
  staged?: number;
  unstaged?: number;
  untracked?: number;
  conflicted?: number;
  lastCommit?: { hash: string; subject: string; at: string };
  error?: string;
}

/** Which side of a file's change a diff request refers to. */
export type GitDiffScope = "staged" | "worktree" | "untracked";

/** One changed file in `git status`. `x`/`y` are the porcelain staged/worktree
 *  status letters ("." = unchanged on that side, "?" = untracked). */
export interface GitFileEntry {
  path: string;
  origPath?: string;
  x: string;
  y: string;
  kind: "changed" | "untracked" | "conflicted";
}

export interface GitStatusData {
  repoId: string;
  branch: string;
  detached: boolean;
  upstream?: string | null;
  ahead: number;
  behind: number;
  staged: number;
  unstaged: number;
  untracked: number;
  conflicted: number;
  entries: GitFileEntry[];
}

export interface GitDiffData {
  path: string;
  unified: string;
  truncated: boolean;
  binary: boolean;
}

export interface GitBranchesData {
  current: string;
  detached: boolean;
  /** Local heads. */
  branches: string[];
  /** Branches that exist only on a remote (name with the remote prefix
   *  stripped) — checking one out creates the local tracking branch. */
  remoteBranches?: string[];
}

/** One commit on master that the running build is missing. */
export interface UpdateCommit {
  hash: string;
  date: string;
  subject: string;
}

/** One pull / rebuild / restart, from claim to completion. `ok` is null while
 *  it is still running, which is also how the UI knows to show a live stage. */
export interface UpdateOperation {
  id: string;
  kind: "pull" | "rebuild" | "restart";
  stage: string;
  startedAt: string;
  finishedAt?: string | null;
  ok?: boolean | null;
  error?: string | null;
  logPath?: string | null;
}

/** Whether a newer origin/master exists than the build that is running.
 *  Three independent kinds of stale: the checkout is behind master (fix =
 *  pull), the checkout is current but the binary predates it (fix = rebuild),
 *  or a newer binary is already on disk (fix = restart). */
export interface UpdateStatus {
  /** False when this build came from outside a git checkout — hide the section. */
  repoAvailable: boolean;
  repoPath: string;
  /** How repoPath was found, so a machine on the wrong checkout is diagnosable.
   *  Optional like the fields below it: a routed answer from a peer still on an
   *  older Threadknot simply will not contain them, and pretending otherwise makes
   *  the UI render confident nonsense about that machine. */
  repoSource?: "env" | "configured" | "embedded" | "executable" | "none";
  /** Baked in at compile time, e.g. "0.1.67" or "0.1.56-dev4". */
  runningVersion: string;
  masterVersion?: string | null;
  /** The one signal that drives the gear pulse. */
  updateAvailable: boolean;
  /** Commits master has that the running build does not. */
  behindBy: number;
  /** Checkout is already at master; only a rebuild is missing. */
  rebuildPending: boolean;
  /** A newer binary is already built; only a restart is missing. Wins over
   *  rebuildPending, since rebuilding again just reproduces it. */
  restartPending?: boolean;
  restartSupported?: boolean;
  rebuildSupported?: boolean;
  /** Executable a restart would launch. */
  binaryPath?: string;
  /** Threads mid-turn or awaiting approval; restarting interrupts them. */
  activeWork?: number;
  branch: string;
  headBehind: number;
  headAhead: number;
  dirty: boolean;
  /** False means we refuse to move the checkout automatically. */
  canFastForward: boolean;
  commits: UpdateCommit[];
  checkedAt: string;
  error?: string | null;
  operation?: UpdateOperation | null;
}

/** Per-repo outcome of a cross-repo operation (commitMany / checkoutMany). */
export interface GitOpResult {
  repoId: string;
  ok: boolean;
  hash?: string;
  subject?: string;
  /** checkoutMany: true = branch was created, false = switched to existing. */
  created?: boolean;
  error?: string;
}

/** A localhost dev server the browser pane can suggest (ports.scan). */
export interface PortInfo {
  port: number;
  url: string;
  title?: string;
}

/** A project-bound terminal tab. Persists across restarts and is renameable;
 *  `alive` reflects whether its shell process is currently running. */
export interface TermInfo {
  id: string;
  name: string;
  alive: boolean;
  createdAt: string;
}

export interface ListDirData {
  path: string;
  parent: string | null;
  entries: DirEntry[];
}

/** A user-crafted appearance theme (Settings → Appearance): a named palette
 *  starting from a preset base + accent, with optional neutral-slot overrides
 *  and an optional background image. Stored server-side (a data-URL background
 *  overflows the localStorage budget) and machine-local (not mesh-replicated).
 *  The server owns `createdAt`/`updatedAt`. */
export interface CustomTheme {
  id: string;
  name: string;
  /** Preset palette the theme starts from (a THEMES id, e.g. "midnight"). */
  base: string;
  /** Accent preset id or #rrggbb. */
  accent: string;
  /** Optional overrides for the neutral palette slots; keys are CSS var names
   *  WITHOUT the leading --, e.g. "bg", "panel", "text". */
  colors?: Record<string, string>;
  /** Optional background image as a data URL. */
  backgroundImage?: string;
  /** 0..0.9 dim laid over the background image so content stays readable. */
  backgroundDim?: number;
  /** Wallpaper zoom, 1..3 (1 = plain cover). Absent = 1. */
  backgroundZoom?: number;
  /** Horizontal pan, -100..100 (0 = centered); percent of the available
   *  overflow in that axis. Absent = 0. */
  backgroundX?: number;
  /** Vertical pan, -100..100 (0 = centered); percent of the available overflow
   *  in that axis. Absent = 0. */
  backgroundY?: number;
  createdAt: string;
  updatedAt: string;
}

/** A palette designed by the local Claude CLI from a wallpaper. */
export interface AiPalette {
  /** "dark" | "light" — which family the scheme is built for. */
  family: string;
  accent: string;
  /** The 10 neutral slots keyed like `CustomTheme.colors`:
   *  bg, bg-raise, panel, panel-2, panel-3, line, line-2, text, dim, faint. */
  colors: Record<string, string>;
  /** A short display name the AI suggests for the theme. */
  name?: string;
}

// ---- request map: type -> { payload, data } -----------------------------

/** A registered remote Hermes gateway (Settings → hermes agents). The API key
 *  is write-only: it is sent on add and never returned. */
export interface HermesAgentInfo {
  id: string;
  name: string;
  image?: string;
  /** User-set profile photo (hermes.agent.setAvatar); wins over `image`. */
  avatar?: string;
  baseUrl: string;
  /** Model (profile) name the gateway advertises. */
  model: string;
  createdAt: string;
}

/** Live Online/Offline presence for one registered Hermes gateway, keyed by
 *  its registry `agentId`. Maintained by the server's background health poller;
 *  fetched via `hermes.agent.statuses` on connect and kept fresh by the
 *  `hermes.statuses` broadcast. `latencyMs`/`version` are present only when the
 *  gateway is online. */
export interface HermesAgentStatus {
  agentId: string;
  /** ISO time of the most recent probe. Truthful, but bumps every poll — not a
   *  "live freshness" signal, so the UI does not present it as one. */
  lastCheckedAt: string;
  online: boolean;
  /** ISO time the CURRENT online/offline state was entered (preserved across
   *  polls that do not flip it); drives "offline since …". Absent only from a
   *  pre-`sinceAt` server. */
  sinceAt: string;
  latencyMs?: number;
  version?: string;
}

export interface HermesToolset {
  name: string;
  label: string;
  description: string;
  enabled: boolean;
  configured?: boolean;
  tools: string[];
}

export interface HermesSkill {
  name: string;
  description: string;
  category?: string | null;
}

export interface HermesAgentDetails {
  health: { ok: boolean; version?: string | null };
  skills: HermesSkill[];
  /** Includes MCP-mounted toolsets — that's how hermes surfaces MCP servers. */
  toolsets: HermesToolset[];
}

/** One extra environment variable for a Claudex profile's `claude` process.
 *  A `sensitive` entry comes back with `value: null` — the server keeps it. */
export interface ClaudexEnvVar {
  name: string;
  value: string | null;
  sensitive: boolean;
}

/** A local bridge process Threadknot starts on demand for a profile. */
export interface ClaudexSidecar {
  command: string;
  args: string[];
}

export type ClaudexSidecarState = "external" | "managed" | "stopped";

/** A Claudex profile (Settings → claudex profiles). Secrets are write-only:
 *  `authToken` is sent on add/update and never returned — `hasAuthToken` says
 *  whether one is stored. Omitting it on update keeps the stored value. */
export interface ClaudexProfileInfo {
  id: string;
  name: string;
  avatar?: string | null;
  baseUrl: string;
  /** Model id as the GATEWAY names it, e.g. `gpt-5.6-sol`. */
  model: string;
  smallModel?: string | null;
  /** Real upstream context window; drives the composer's readout. */
  contextWindow?: number | null;
  efforts: string[];
  defaultEffort?: string | null;
  hasAuthToken: boolean;
  env: ClaudexEnvVar[];
  sidecar?: ClaudexSidecar | null;
  createdAt: string;
}

/** Fields accepted when creating or editing a profile. */
export interface ClaudexProfileInput {
  name?: string;
  baseUrl?: string;
  model?: string;
  smallModel?: string;
  contextWindow?: number;
  efforts?: string[];
  defaultEffort?: string;
  /** Omit to keep the stored token; empty string clears it. */
  authToken?: string;
  env?: { name: string; value: string; sensitive?: boolean }[];
  /** `null` removes the sidecar; omit to leave it unchanged. */
  sidecar?: ClaudexSidecar | null;
}

/** What a paired device is allowed to do. Chosen by the desktop owner, stored
 *  server-side against the device, and enforced there — this list is what the
 *  UI shows, never what it asserts. See `mobile.rs` / `Capability`. */
export type DeviceCapability =
  | "threads"
  | "files"
  | "git"
  | "terminal"
  | "browser"
  | "signedBrowser"
  | "mesh";

/** Grants a pairing starts with unless the owner narrows it. Mirrors
 *  `default_capabilities()` — notably WITHOUT `signedBrowser`. */
export const DEFAULT_DEVICE_CAPABILITIES: DeviceCapability[] = [
  "threads",
  "files",
  "git",
  "terminal",
  "browser",
  "mesh",
];

/** Label + consequence for each grant, in the order the settings UI lists them. */
export const DEVICE_CAPABILITY_LABELS: {
  id: DeviceCapability;
  label: string;
  detail: string;
}[] = [
  { id: "threads", label: "chats", detail: "read chats, send turns, answer approvals" },
  { id: "files", label: "files", detail: "browse and download project files" },
  { id: "git", label: "git", detail: "see and act on repository state" },
  { id: "terminal", label: "terminals", detail: "a real shell on this machine" },
  { id: "browser", label: "browser", detail: "drive throwaway browser sessions" },
  {
    id: "signedBrowser",
    label: "signed-in browser",
    detail: "act as your logged-in accounts — off by default",
  },
  { id: "mesh", label: "other machines", detail: "use the grants above on paired machines" },
];

/** Where a pairing QR sends the phone. `lan` is the local address; `remote` is
 *  the provisioned public origin, and is only offered once remote access is
 *  configured and on. */
export type PairingTarget = "lan" | "remote";

/** The hosted relay connector for THIS machine (Stage 2). */
export interface ConnectorStatus {
  /** `off` | `unenrolled` | `connecting` | `online` | `error`. `unenrolled`
   *  exists so that "on but no token pasted yet" does not read as
   *  "connecting" forever, which is the state people file support tickets about. */
  state: "off" | "unenrolled" | "connecting" | "online" | "error";
  /** Server-assigned. Never chosen by this machine. */
  hostname: string;
  publicOrigin: string;
  lastError?: string;
  connectedSince?: string;
  bytesIn: number;
  bytesOut: number;
  /** False when a subscription has lapsed or a kill switch is on. Existing
   *  traffic keeps flowing; only new sessions are refused. */
  acceptingNewSessions: boolean;
  holdReason?: string;
  /** Days left in the trial. Present so the panel can warn *before* anything
   *  stops working — `holdReason` only appears once the hold is already in
   *  force, so a build that watched only that told people on the day it broke. */
  trialDaysLeft?: number;
  /** A connection request waiting for someone to approve it in the console.
   *  Present only while one is in flight. */
  approval?: ConnectorApproval;
  monthBytes?: number;
  monthQuotaBytes?: number;
  liveStreams: number;
}

/** A device-approval request in flight.
 *
 *  Note what is absent: the `deviceCode`. That is the secret which collects the
 *  enrollment and it never leaves the Rust side, so nothing here is damaging to
 *  read over a shoulder or in a screenshot. */
export interface ConnectorApproval {
  /** Hyphenated, for reading aloud. A fallback — the normal path is the link. */
  userCode: string;
  verificationUri: string;
  /** The approval page with the code already filled in. */
  verificationUriComplete: string;
  expiresAt: string;
  /** Approval is not a state: a granted request becomes an enrollment and the
   *  panel disappears on its own. */
  state: "waiting" | "denied" | "expired";
}

export interface RemoteAccess {
  /** Off by default. Turning it off signs every remote browser out at once. */
  enabled: boolean;
  /** The provisioned public origin, e.g. `https://x.remote.threadknot.ai`. */
  origin?: string | null;
  /** Loopback port the connector dials. Never reachable from the network. */
  loopbackPort: number;
  /** How many browser sessions are currently outstanding. */
  browserSessions?: number;
}

/** A phone paired with this server (Settings → mobile devices). */
export interface MobileDeviceInfo {
  id: string;
  name: string;
  platform: string;
  expoPushToken?: string;
  notificationsEnabled: boolean;
  notifyErrors: boolean;
  /** Absent only from a server older than capability-scoped pairing. */
  capabilities?: DeviceCapability[];
  capabilitiesVersion?: number;
  createdAt: string;
  lastSeenAt?: string;
}

/** A one-time pairing code rendered as a QR for the mobile app to scan. The
 *  payload carries the code, never the master token — see `mobile.rs`. */
export interface PairingQr {
  /** `threadknot://pair?u=…&c=…` — what the QR actually encodes. */
  payload: string;
  /** Standalone SVG markup for the code. */
  qrSvg: string;
  /** Human-readable `XXXXX-XXXXX` fallback if the camera won't cooperate. */
  code: string;
  /** This server's origin, shown so it's clear which machine is on offer. */
  url: string;
  /** Grants this code will hand the phone that redeems it. */
  capabilities?: DeviceCapability[];
  /** Which address the QR points at. */
  target?: PairingTarget;
  /** Seconds the code stays redeemable from when it was minted. */
  ttlSeconds: number;
}

export interface RequestMap {
  hello: { payload: Record<string, never>; data: HelloData };
  "app.changelog": {
    payload: Record<string, never>;
    data: { entries: ChangelogEntry[]; notes: ReleaseNote[] };
  };
  /** Browser logins live on the machine whose Chrome holds the session, so
   *  all four route: `machineId` names the machine being managed (omitted
   *  means this one). All but `list` require that machine's master token. */
  "browser.profile.list": {
    payload: { machineId?: string };
    data: { profiles: BrowserProfileInfo[] };
  };
  "browser.profile.create": {
    payload: { name: string; origins: string[]; machineId?: string };
    data: BrowserProfileInfo;
  };
  "browser.profile.update": {
    payload: { profileId: string; name?: string; origins?: string[]; machineId?: string };
    data: BrowserProfileInfo;
  };
  "browser.profile.delete": {
    payload: { profileId: string; machineId?: string };
    data: Record<string, never>;
  };
  /** The Library. Like browser profiles these are per-machine and all route on
   *  `machineId`; everything but `list` needs that machine's master token,
   *  because installing a server hands its agents a new tool to run. */
  "library.list": { payload: { machineId?: string }; data: LibraryData };
  /** Install a skill folder into each named agent's skills directory. Give
   *  either a `catalogId` or a `source` (GitHub URL or `owner/repo/path`). */
  "library.skill.install": {
    payload: {
      agents: SkillTarget[];
      catalogId?: string;
      source?: string;
      /** Override the folder name; defaults to the last path segment. */
      name?: string;
      machineId?: string;
    };
    data: { skill: InstalledSkill };
  };
  /** Spread a skill that is already installed somewhere into other agents'
   *  directories — the only way to share hand-written or plugin skills, which
   *  have no upstream URL to re-fetch. */
  "library.skill.copy": {
    payload: { skillId: string; agents: SkillTarget[]; machineId?: string };
    data: { copied: number };
  };
  "library.skill.remove": {
    payload: { skillId: string; agents: SkillTarget[]; machineId?: string };
    data: { removed: number };
  };
  /** Create or replace a server by hand (matched by `id`; empty id mints one). */
  "library.mcp.save": {
    payload: { server: McpServerInfo; machineId?: string };
    data: { server: McpServerInfo };
  };
  /** Install a catalog server, filling its `${…}` placeholders from `inputs`. */
  "library.mcp.install": {
    payload: {
      catalogId: string;
      inputs?: Record<string, string>;
      name?: string;
      agents?: Agent[];
      machineId?: string;
    };
    data: { server: McpServerInfo };
  };
  "library.mcp.delete": {
    payload: { serverId: string; machineId?: string };
    data: Record<string, never>;
  };
  /** Mint a one-time pairing code + its QR. Master-only: a paired phone must
   *  not be able to bring in more phones. */
  "mobile.pair.begin": {
    /** Grants bound to the code server-side; the phone cannot widen them.
     *  `target` picks the address the QR carries — the remote one comes from
     *  stored configuration, never from the request's Host header. */
    payload: { capabilities?: DeviceCapability[]; target?: PairingTarget };
    data: PairingQr;
  };
  /** Invalidate outstanding codes when the QR leaves the screen. */
  "mobile.pair.cancel": { payload: Record<string, never>; data: { ok: boolean } };
  "mobile.device.list": {
    payload: Record<string, never>;
    data: { devices: MobileDeviceInfo[] };
  };
  "mobile.device.revoke": {
    payload: { deviceId: string };
    data: Record<string, never>;
  };
  /** Master-only. Reducing a grant also drops that device's live sockets. */
  "mobile.device.setCapabilities": {
    payload: { deviceId: string; capabilities: DeviceCapability[] };
    data: { device: MobileDeviceInfo };
  };
  /** Master-only. */
  "connector.status": { payload: Record<string, never>; data: ConnectorStatus };
  "connector.enroll": {
    payload: { enrollmentToken: string; machineName?: string };
    data: ConnectorStatus;
  };
  /** Master-only. Opens a device-approval request; the connector then polls for
   *  the answer on its own, so closing this panel does not abandon it. */
  "connector.beginApproval": {
    payload: { machineName?: string };
    data: ConnectorApproval;
  };
  "connector.cancelApproval": {
    payload: Record<string, never>;
    data: ConnectorStatus;
  };
  "connector.setEnabled": {
    payload: { enabled: boolean };
    data: ConnectorStatus;
  };
  "remote.get": { payload: Record<string, never>; data: RemoteAccess };
  /** Master-only. Absent keys are left alone; `origin: null` clears it. */
  "remote.set": {
    payload: { enabled?: boolean; origin?: string | null };
    data: RemoteAccess;
  };
  "hermes.agent.list": {
    payload: Record<string, never>;
    data: { agents: HermesAgentInfo[] };
  };
  "hermes.agent.add": {
    payload: { baseUrl: string; apiKey: string; name?: string };
    data: HermesAgentInfo;
  };
  "hermes.agent.remove": { payload: { agentId: string }; data: Record<string, never> };
  "hermes.agent.statuses": {
    payload: Record<string, never>;
    data: { revision: number; statuses: HermesAgentStatus[] };
  };
  "hermes.agent.setImage": {
    payload: { agentId: string; image: string | null };
    data: HermesAgentInfo;
  };
  "hermes.agent.setAvatar": {
    payload: { agentId: string; image: string | null };
    data: HermesAgentInfo;
  };
  "hermes.agent.details": { payload: { agentId: string }; data: HermesAgentDetails };
  "claudex.profile.list": {
    payload: Record<string, never>;
    data: { profiles: ClaudexProfileInfo[] };
  };
  "claudex.profile.add": { payload: ClaudexProfileInput; data: ClaudexProfileInfo };
  "claudex.profile.update": {
    payload: ClaudexProfileInput & { profileId: string };
    data: ClaudexProfileInfo;
  };
  "claudex.profile.remove": { payload: { profileId: string }; data: Record<string, never> };
  "claudex.profile.setAvatar": {
    payload: { profileId: string; image: string | null };
    data: ClaudexProfileInfo;
  };
  /** Reachability check that also STARTS a configured sidecar. */
  "claudex.profile.test": {
    payload: { profileId: string };
    data: { sidecar: ClaudexSidecarState; baseUrl: string };
  };
  "claudex.profile.status": {
    payload: { profileId: string };
    data: { sidecar: ClaudexSidecarState };
  };
  /** User-crafted appearance themes (machine-local). Changes broadcast
   *  `state.changed { scope: "themes" }`; clients answer with a fresh list. */
  "theme.list": { payload: Record<string, never>; data: { themes: CustomTheme[] } };
  /** Upsert by id; the server sets `updatedAt` (and `createdAt` when new). */
  "theme.save": { payload: { theme: CustomTheme }; data: CustomTheme };
  "theme.remove": { payload: { themeId: string }; data: Record<string, never> };
  /** Design a complementary palette from a wallpaper via the local Claude CLI.
   *  Routable: `machineId` names a machine that has the CLI installed. */
  "theme.aiPalette": {
    payload: { imageDataUrl: string; hint?: string; machineId?: string };
    data: AiPalette;
  };
  "device.info": { payload: Record<string, never>; data: DeviceInfo };
  "device.rename": { payload: { name: string; machineId?: string }; data: { friendlyName: string } };
  "device.setAppearance": {
    payload: { image?: string | null; color?: string | null; machineId?: string };
    data: { avatar?: string; color?: string };
  };
  "peer.setAppearance": {
    payload: { machineId: string; image?: string | null; color?: string | null };
    data: PeerInfo;
  };
  "peer.list": {
    payload: Record<string, never>;
    data: { peers: PeerInfo[]; discovered: DiscoveredPeer[] };
  };
  "peer.add": { payload: { url: string; token?: string }; data: PeerInfo };
  "peer.remove": { payload: { machineId: string }; data: Record<string, never> };
  "workspace.list": { payload: Record<string, never>; data: { workspaces: Workspace[] } };
  "workspace.rename": { payload: { workspaceId: string; name: string }; data: Workspace };
  "workspace.setFavorite": {
    payload: { workspaceId: string; favorite: boolean };
    data: Workspace;
  };
  "workspace.setHidden": {
    payload: { workspaceId: string; hidden: boolean };
    data: Workspace;
  };
  "workspace.setImage": {
    payload: { workspaceId: string; image: string | null };
    data: Workspace;
  };
  "workspace.attachRoot": {
    payload: { workspaceId: string; machineId: string; path: string };
    data: { workspace: Workspace; project: Project };
  };
  "workspace.detachRoot": {
    payload: { workspaceId: string; machineId: string; projectId: string };
    data: Workspace;
  };
  "project.create": { payload: { path: string; name?: string }; data: Project };
  "project.list": { payload: Record<string, never>; data: { projects: Project[] } };
  "project.delete": { payload: { projectId: string }; data: Record<string, never> };
  "thread.create": {
    payload: { projectId: string; agent: Agent; settings: ThreadSettings; machineId?: string };
    data: Thread;
  };
  "thread.list": { payload: { projectId: string; machineId?: string }; data: { threads: Thread[] } };
  "thread.get": {
    payload: { threadId: string; machineId?: string };
    data: { thread: Thread; events: PersistedEvent[] };
  };
  "thread.search": {
    payload: { query: string; threadIds: string[]; machineId?: string };
    data: { threadIds: string[] };
  };
  "thread.toolOutput": {
    payload: { threadId: string; callId: string; machineId?: string };
    data: { output: string | null };
  };
  "thread.preview": { payload: { threadId: string; machineId?: string }; data: ThreadPreview };
  "thread.rename": { payload: { threadId: string; title: string; machineId?: string }; data: Thread };
  /** Park a chat in the settled shelf (`settled: true`) or pull it back out
   *  (`settled: false`, which also pins it against idle auto-settle). */
  "thread.setSettled": {
    payload: { threadId: string; settled: boolean; machineId?: string };
    data: Thread;
  };
  "thread.setFavorite": {
    payload: { threadId: string; favorite: boolean; machineId?: string };
    data: Thread;
  };
  "thread.delete": { payload: { threadId: string; machineId?: string }; data: Record<string, never> };
  "thread.setSettings": {
    payload: { threadId: string; settings: ThreadSettings; machineId?: string };
    data: Thread;
  };
  "thread.setAgent": {
    payload: { threadId: string; agent: Agent; settings: ThreadSettings; machineId?: string };
    data: Thread;
  };
  /** Throw a second agent at this thread as an adversarial reviewer, for one
   *  turn. `model` is required for the profile-backed kinds (hermes, claudex);
   *  others fall back to their default. `access` omitted means the reviewer is
   *  read-only; anything else is a deliberate grant of write permissions.
   *  Resolves with the lane. */
  "thread.review": {
    payload: {
      threadId: string;
      agent: Agent;
      model?: string;
      effort?: string;
      access?: Access;
      instructions?: string;
      machineId?: string;
    };
    data: Participant;
  };
  /** Run a structured multi-agent debate (Parley): each round every reviewer
   *  attacks the thread's work, the builder answers the objectors, and the
   *  loop repeats until all concede — then, with `execute`, the builder
   *  implements what it conceded — or `rounds` (clamped 1..=6, default 2) is
   *  hit and the leftovers escalate to the user. A message sent via
   *  `turn.start` mid-parley queues and takes the floor at the next turn
   *  boundary. Errors if busy, empty, or a parley is already running.
   *  Resolves with the updated Thread. */
  "thread.parley.start": {
    payload: {
      threadId: string;
      reviewers: {
        agent: Agent;
        model?: string;
        effort?: string;
        access?: Access;
        /** Lane display name (personas); defaults to "{Agent} (reviewer)". */
        name?: string;
        /** Stable persona identity: two personas on the same setup seat two lanes. */
        personaId?: string;
        /** Folded into every review brief this reviewer runs. */
        personality?: string;
      }[];
      rounds?: number;
      execute?: boolean;
      instructions?: string;
      machineId?: string;
    };
    data: Thread;
  };
  /** Create or replace a reviewer persona (matched by `id`; empty id mints
   *  one). Master only; pulses "identity" so clients refetch hello. */
  "persona.save": {
    payload: { persona: ReviewerPersona };
    data: ReviewerPersona;
  };
  "persona.delete": { payload: { personaId: string }; data: Record<string, never> };
  "thread.archive": { payload: { threadId: string; machineId?: string }; data: { archive: ArchiveHeader } };
  "archive.list": { payload: { machineId?: string }; data: { archives: ArchiveHeader[] } };
  "archive.restore": { payload: { archiveId: string; machineId?: string }; data: { thread: Thread } };
  "archive.delete": { payload: { archiveId: string; machineId?: string }; data: Record<string, never> };
  "settings.get": { payload: Record<string, never>; data: { archiveDir: string } };
  "settings.set": { payload: { archiveDir: string }; data: { archiveDir: string } };
  "turn.start": {
    payload: {
      threadId: string;
      text: string;
      attachments?: OutgoingAttachment[];
      machineId?: string;
    };
    data: Record<string, never>;
  };
  "turn.steer": {
    payload: { threadId: string; text: string; machineId?: string };
    data: Record<string, never>;
  };
  "turn.interrupt": { payload: { threadId: string; machineId?: string }; data: Record<string, never> };
  "approval.respond": {
    payload: { threadId: string; approvalId: string; optionId: string; machineId?: string };
    data: Record<string, never>;
  };
  "question.respond": {
    payload: {
      threadId: string;
      requestId: string;
      answers: Record<string, string[]>;
      machineId?: string;
    };
    data: Record<string, never>;
  };
  "schedule.create": {
    payload: {
      projectId: string;
      agent: Agent;
      settings: ThreadSettings;
      name?: string;
      prompt: string;
      cadence: Cadence;
    };
    data: Schedule;
  };
  "schedule.list": { payload: Record<string, never>; data: { schedules: Schedule[] } };
  "schedule.update": {
    payload: {
      scheduleId: string;
      name?: string;
      prompt?: string;
      cadence?: Cadence;
      enabled?: boolean;
      agent?: Agent;
      settings?: ThreadSettings;
      projectId?: string;
    };
    data: Schedule;
  };
  "schedule.delete": { payload: { scheduleId: string }; data: Record<string, never> };
  "schedule.run": { payload: { scheduleId: string }; data: { threadId: string } };
  "usage.get": { payload: Record<string, never>; data: { usage: ProviderUsage[] } };
  "usage.refresh": { payload: Record<string, never>; data: Record<string, never> };
  "dictation.settings.get": { payload: Record<string, never>; data: DictationSettings };
  "dictation.settings.save": { payload: DictationSettingsInput; data: DictationSettings };
  /** Records the mic on the machine serving this connection — never peer routed. */
  "dictation.start": { payload: Record<string, never>; data: { recordingId: string } };
  /** Empty `text` means the clip held no speech. */
  "dictation.stop": { payload: { recordingId: string }; data: { text: string } };
  "dictation.cancel": { payload: { recordingId: string }; data: Record<string, never> };
  "fs.listDir": { payload: { path?: string; machineId?: string }; data: ListDirData };
  "fs.tree": { payload: { projectId: string; machineId?: string }; data: FileTreeData };
  "fs.read": {
    payload: { projectId: string; path: string; machineId?: string };
    data: FileReadData;
  };
  "artifacts.list": {
    payload: { projectId: string; threadId?: string; machineId?: string };
    data: { artifacts: ArtifactRecord[] };
  };
  "artifacts.delete": { payload: { artifactId: string; machineId?: string }; data: Record<string, never> };
  "git.repos": { payload: { projectId: string; machineId?: string }; data: { repos: GitRepoInfo[] } };
  "git.status": { payload: { repoId: string; machineId?: string }; data: GitStatusData };
  "git.diff": {
    payload: { repoId: string; path: string; scope: GitDiffScope; machineId?: string };
    data: GitDiffData;
  };
  "git.stage": { payload: { repoId: string; paths: string[]; machineId?: string }; data: GitStatusData };
  "git.unstage": { payload: { repoId: string; paths: string[]; machineId?: string }; data: GitStatusData };
  "git.discard": { payload: { repoId: string; paths: string[]; machineId?: string }; data: GitStatusData };
  "git.commit": {
    payload: { repoId: string; message: string; machineId?: string };
    data: GitStatusData & { hash: string; subject: string };
  };
  "git.branches": { payload: { repoId: string; machineId?: string }; data: GitBranchesData };
  "git.checkout": {
    payload: { repoId: string; branch: string; create?: boolean; machineId?: string };
    data: GitStatusData;
  };
  "git.commitMany": {
    payload: {
      projectId: string;
      entries: { repoId: string; message: string; stageAll?: boolean }[];
      link?: boolean;
      machineId?: string;
    };
    data: { results: GitOpResult[]; changeId?: string | null };
  };
  "git.checkoutMany": {
    payload: { projectId: string; repoIds: string[]; branch: string; machineId?: string };
    data: { results: GitOpResult[] };
  };
  "git.pr": {
    payload: { repoId: string; title?: string; body?: string; machineId?: string };
    data: { url?: string | null; output: string };
  };
  "git.push": { payload: { repoId: string; machineId?: string }; data: GitStatusData & { output: string } };
  "git.pull": { payload: { repoId: string; machineId?: string }; data: GitStatusData & { output: string } };
  // Threadknot updating itself. No repoId: these act on Threadknot's own checkout.
  // Routed by machineId like the rest of the git family, so the settings tab can
  // ask every peer the same question.
  "git.selfUpdateStatus": {
    payload: { fetch?: boolean; machineId?: string };
    data: UpdateStatus;
  };
  "git.selfUpdateCheck": { payload: { machineId?: string }; data: Record<string, never> };
  "git.selfUpdatePull": { payload: { machineId?: string }; data: { ok: boolean } };
  /** Returns as soon as the build is claimed; progress arrives on the
   *  `updates` broadcast, because a release compile outlives any request. */
  "git.selfUpdateRebuild": {
    payload: { machineId?: string };
    data: { ok: boolean; operationId: string };
  };
  /** `force` overrides the active-work guard. */
  "git.selfUpdateRestart": {
    payload: { machineId?: string; force?: boolean };
    data: { ok: boolean; restarting?: boolean };
  };
  /** Machine-local source-folder override; never replicated to peers. */
  "git.selfUpdateSetRepoPath": {
    payload: { path: string; machineId?: string };
    data: { ok: boolean };
  };
  "ports.scan": { payload: Record<string, never>; data: { ports: PortInfo[] } };
  "term.list": { payload: { projectId: string; machineId?: string }; data: { terms: TermInfo[] } };
  "term.create": { payload: { projectId: string; name?: string; machineId?: string }; data: TermInfo };
  "term.rename": { payload: { termId: string; name: string; machineId?: string }; data: TermInfo };
  "term.delete": { payload: { termId: string; machineId?: string }; data: Record<string, never> };
}

export type RequestType = keyof RequestMap;

// ---- server frames ------------------------------------------------------

export interface EventFrame {
  type: "event";
  threadId: string;
  seq: number;
  /** Authoritative server time. Optional while talking to an older peer. */
  ts?: string;
  /** Producing lane; see `PersistedEvent.speaker`. */
  speaker?: string;
  event: AgentEvent;
}

export interface StateChangedFrame {
  type: "state.changed";
  scope:
    | "projects"
    | "workspaces"
    | "peers"
    | "threads"
    | "schedules"
    | "terminals"
    | "artifacts"
    | "git"
    | "agents"
    | "archives"
    | "identity"
    | "themes"
    | "updates";
  projectId?: string;
  /** Set on frames relayed from a peer: the origin machine's id (added by the
   *  peer-socket relay). Absent on locally produced frames. Lets a machine-
   *  scoped refresh (e.g. archives) target the peer whose state changed. */
  origin?: string;
}

export interface ResponseFrame {
  type: "response";
  id: number;
  ok: boolean;
  data?: unknown;
  error?: string;
}

export interface UsageFrame {
  type: "usage";
  usage: ProviderUsage[];
}

/** Pushed when a Hermes gateway's live presence changes (online<->offline, or
 *  an agent added/removed). Carries the full fresh snapshot plus its
 *  `revision`, a per-server-process counter the client uses to drop a frame
 *  that lost a delivery race with a fresher one. */
export interface HermesStatusesFrame {
  type: "hermes.statuses";
  revision: number;
  statuses: HermesAgentStatus[];
}

export type ServerFrame =
  | EventFrame
  | StateChangedFrame
  | ResponseFrame
  | UsageFrame
  | HermesStatusesFrame;
