import { createContext, useContext } from "react";
import type {
  Access,
  Agent,
  AgentEvent,
  AgentModel,
  AiPalette,
  ArtifactRecord,
  ArchiveHeader,
  CustomTheme,
  DiscoveredPeer,
  HelloData,
  HermesAgentInfo,
  HermesAgentStatus,
  PeerInfo,
  PersistedEvent,
  Project,
  ProviderUsage,
  ReviewerPersona,
  Schedule,
  TermInfo,
  Thread,
  ThreadSettings,
  ThreadStatus,
  Workspace,
} from "../lib/protocol";
import { isQuickHomeProjectId } from "../lib/protocol";
import type { ConnState } from "../lib/ws";
import { isAgentVisible } from "../lib/agentVisibility";
import { applyEvent, type FeedItem } from "./feed";

export const LS_LAST_THREAD = "threadknot.lastThread";
const LS_NEW_THREAD_SETTINGS = "threadknot.newThreadSettings";
export const LS_THREAD_ATTENTION = "threadknot.threadAttention";
const NEW_THREAD_DEFAULTS_VERSION = 3;
const RETIRED_CLAUDE_OPUS_MODEL = "claude-opus-4-8";
const CURRENT_CLAUDE_OPUS_MODEL = "claude-opus-5";

/** Completion/read state belongs to this client, not the server: opening a
 * chat on one person's phone must not mark it read on everyone else's. */
export function loadThreadAttention(): Record<string, true> {
  try {
    if (typeof localStorage === "undefined") return {};
    const parsed: unknown = JSON.parse(localStorage.getItem(LS_THREAD_ATTENTION) ?? "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(
        ([threadId, unread]) => threadId.length > 0 && unread === true,
      ),
    );
  } catch {
    return {};
  }
}

export function persistThreadAttention(attention: Record<string, true>): void {
  try {
    localStorage.setItem(LS_THREAD_ATTENTION, JSON.stringify(attention));
  } catch {
    // Read markers are a convenience; locked-down storage must not block chat.
  }
}

/** localStorage key for the last-open thread; solo windows get their own slot
 *  so they don't stomp the fleet window's restore state (localStorage is
 *  shared across all windows of the app). */
export function lastThreadKey(solo: string | null): string {
  return solo ? `${LS_LAST_THREAD}.${solo}` : LS_LAST_THREAD;
}

/** New-thread choices are project-scoped and shared by fleet/solo windows. */
function newThreadSettingsKey(projectId: string): string {
  return `${LS_NEW_THREAD_SETTINGS}.${projectId}`;
}

export type WorkspaceTab = "files" | "git" | "artifacts" | "browser" | "terminal";

export interface DraftThread {
  projectId: string;
  /** Machine the new thread will live on (== hello.machineId for local). */
  machineId: string;
  agent: Agent;
  settings: ThreadSettings;
}

/** In-app toast (chat finished / awaiting input); click focuses the thread. */
export interface Notice {
  id: number;
  threadId: string;
  title: string;
  body: string;
}

export interface AppState {
  conn: ConnState;
  isTauri: boolean;
  /** Non-null → this window is dedicated to one project (dragged out of the
   *  sidebar); holds that project's id. */
  solo: string | null;
  /** Project row currently being dragged out of the sidebar (shows the
   *  "open in new window" drop zone). */
  dragProject: Project | null;
  /** HTTP base + token for token-gated requests (attachment thumbnails). */
  /** `token` is empty in remote mode, where authentication is a cookie; `csrf`
   *  is the double-submit half a cookie session must send with state changes. */
  http: { base: string; token: string; csrf?: string } | null;
  hello: HelloData | null;
  projects: Project[];
  /** Sidebar top-level containers (NOT the `workspace` tab map below —
   *  that's the per-project side-panel state, an unrelated concept). */
  workspaces: Workspace[];
  /** Paired peer machines with live presence (Settings → machines; later the
   *  machine picker + chips). */
  peers: PeerInfo[];
  /** Unpaired Threadknots discovered on the LAN via mDNS. */
  discovered: DiscoveredPeer[];
  /** Registered Hermes gateways (avatars for sidebar rows + chat headers). */
  hermesAgents: HermesAgentInfo[];
  /** Live Online/Offline presence per registered Hermes gateway, keyed by
   *  agentId. Seeded from `hermes.agent.statuses` on connect and kept fresh by
   *  the `hermes.statuses` broadcast; an agent absent from the map has no
   *  known status yet. */
  hermesStatuses: Record<string, HermesAgentStatus>;
  /** User-crafted appearance themes, stored server-side. Seeded on connect and
   *  kept fresh by the `state.changed { scope: "themes" }` broadcast. */
  customThemes: CustomTheme[];
  /** False until the first `theme.list` round trip has populated
   *  `customThemes`. ThemeSync gates on this so a crafted theme's id is never
   *  treated as "deleted elsewhere" (and permanently cleared) merely because
   *  the records have not loaded yet on boot. */
  themesLoaded: boolean;
  /** Highest presence `revision` applied so far. The server's revision is
   *  per-process, so this resets to -1 on every (re)connect (see the "conn"
   *  reducer) before the reconnect re-requests the snapshot; a frame or
   *  snapshot with `revision <= hermesRevision` is stale and dropped. */
  hermesRevision: number;
  threads: Record<string, Thread[]>; // keyed by projectId
  collapsed: Record<string, boolean>; // projectId -> collapsed
  activeThreadId: string | null;
  draft: DraftThread | null;
  feed: FeedItem[];
  feedThreadId: string | null; // which thread the feed belongs to
  feedLoading: boolean;
  lastSeq: number;
  sidebarOpen: boolean;
  usage: ProviderUsage[];
  notices: Notice[];
  /** Open workspace tab (Files / Browser / Terminal side panel) per project;
   *  a project absent from the map has its panel closed. */
  workspace: Record<string, WorkspaceTab>;
  schedules: Schedule[];
  /** Persisted terminal tabs, keyed by projectId (loaded on demand). */
  terminals: Record<string, TermInfo[]>;
  /** Produced artifacts per project (loaded on demand for the Artifacts tab). */
  artifacts: Record<string, ArtifactRecord[]>;
  /** Artifact id the Artifacts pane should open in its viewer (deep link from
   *  a chat artifact card); consumed and cleared by ArtifactsPane. */
  artifactFocus: string | null;
  /** Discovered git repos per project (fleet summaries for the Git tab). */
  git: Record<string, import("../lib/protocol").GitRepoInfo[]>;
  /** Archived (exported + removed) sessions per machine, newest first within
   *  each machine. Keyed by the owning machineId; the local machine's own
   *  archives live under `hello.machineId`. Archives live on the machine whose
   *  archiveDir holds them, so a fleet view pulls each peer's list separately. */
  archives: Record<string, ArchiveHeader[]>;
  /** Whether a newer origin/master exists than this running build. Null until
   *  the first answer arrives. Drives the settings-gear pulse. */
  update: import("../lib/protocol").UpdateStatus | null;
  /** Thread ids with an unseen moment worth attention — a turn finished, an
   *  approval/question is pending, or a turn errored — while the thread was
   *  NOT the one on screen. Cleared when the thread is opened and persisted
   *  client-side so refreshes do not lose the unread marker. */
  attention: Record<string, true>;
}

export const initialState: AppState = {
  conn: "connecting",
  isTauri: false,
  solo: null,
  dragProject: null,
  http: null,
  hello: null,
  projects: [],
  workspaces: [],
  peers: [],
  discovered: [],
  hermesAgents: [],
  hermesStatuses: {},
  hermesRevision: -1,
  customThemes: [],
  themesLoaded: false,
  threads: {},
  collapsed: {},
  activeThreadId: null,
  draft: null,
  feed: [],
  feedThreadId: null,
  feedLoading: false,
  lastSeq: 0,
  sidebarOpen: false,
  usage: [],
  notices: [],
  workspace: {},
  schedules: [],
  terminals: {},
  artifacts: {},
  artifactFocus: null,
  git: {},
  archives: {},
  update: null,
  attention: loadThreadAttention(),
};

export type Action =
  | { type: "conn"; conn: ConnState }
  | { type: "isTauri"; value: boolean }
  | { type: "solo"; projectId: string | null }
  | { type: "dragProject"; project: Project | null }
  | { type: "http"; value: { base: string; token: string; csrf?: string } }
  | { type: "hello"; data: HelloData }
  | { type: "projects"; projects: Project[] }
  | { type: "workspaces"; workspaces: Workspace[] }
  | { type: "peers"; peers: PeerInfo[]; discovered: DiscoveredPeer[] }
  | { type: "hermesAgents"; agents: HermesAgentInfo[] }
  | { type: "hermesStatuses"; revision: number; statuses: HermesAgentStatus[] }
  | { type: "customThemes"; themes: CustomTheme[] }
  | { type: "threads"; projectId: string; threads: Thread[] }
  | { type: "toggleProject"; projectId: string }
  | { type: "openThread"; threadId: string }
  | { type: "openDraft"; draft: DraftThread }
  | { type: "closeActive" }
  | { type: "feedLoaded"; threadId: string; thread: Thread; events: PersistedEvent[] }
  | {
      type: "agentEvent";
      threadId: string;
      seq: number;
      timestamp: string;
      /** Producing lane, when the server attributed the event to one. */
      speaker?: string;
      event: AgentEvent;
    }
  | { type: "threadUpserted"; thread: Thread }
  | { type: "threadDeleted"; threadId: string; projectId: string }
  | { type: "projectDeleted"; projectId: string }
  | { type: "draftSettings"; agent: Agent; settings: ThreadSettings }
  | { type: "approvalPending"; approvalId: string }
  | { type: "questionAnswer"; requestId: string; answers: Record<string, string[]> }
  | { type: "questionPending"; requestId: string }
  | { type: "sidebar"; open: boolean }
  | { type: "usage"; usage: ProviderUsage[] }
  | { type: "noticeAdd"; notice: Notice }
  | { type: "noticeDismiss"; id: number }
  | { type: "workspace"; projectId: string; tab: WorkspaceTab | null }
  | { type: "schedules"; schedules: Schedule[] }
  | { type: "terminals"; projectId: string; terminals: TermInfo[] }
  | { type: "artifacts"; projectId: string; artifacts: ArtifactRecord[] }
  | { type: "artifactFocus"; artifactId: string | null }
  | { type: "git"; projectId: string; repos: import("../lib/protocol").GitRepoInfo[] }
  | { type: "archives"; machineId: string; archives: ArchiveHeader[] }
  | { type: "update"; update: import("../lib/protocol").UpdateStatus | null }
  | { type: "attentionSync"; attention: Record<string, true> }
  | { type: "attentionClear"; threadId: string };

function statusFromEvent(ev: AgentEvent): ThreadStatus | null {
  switch (ev.kind) {
    case "turn_started":
      return "running";
    case "approval_request":
    case "question_request":
      return "waiting_approval";
    case "approval_resolved":
    case "question_resolved":
      return "running";
    case "turn_completed":
    case "turn_aborted":
    case "error":
      return "idle";
    default:
      return null;
  }
}

function sortThreads(list: Thread[]): Thread[] {
  return [...list].sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
}

/** Events that make an off-screen thread worth flagging in the sidebar. */
function isAttentionEvent(ev: AgentEvent): boolean {
  return (
    ev.kind === "turn_completed" ||
    ev.kind === "approval_request" ||
    ev.kind === "question_request" ||
    ev.kind === "error"
  );
}

function patchThread(
  threads: Record<string, Thread[]>,
  threadId: string,
  patch: (t: Thread) => Thread,
): Record<string, Thread[]> {
  for (const pid of Object.keys(threads)) {
    const list = threads[pid];
    const idx = list.findIndex((t) => t.id === threadId);
    if (idx >= 0) {
      const next = list.slice();
      next[idx] = patch(list[idx]);
      return { ...threads, [pid]: sortThreads(next) };
    }
  }
  return threads;
}

export function findThread(state: AppState, threadId: string): Thread | null {
  for (const pid of Object.keys(state.threads)) {
    const hit = state.threads[pid].find((t) => t.id === threadId);
    if (hit) return hit;
  }
  return null;
}

/** The sidebar container a project belongs to. Migration wrapped each project
 *  1:1 reusing its id, so the project id is a sound fallback. */
export function workspaceIdForProject(
  state: AppState,
  projectId: string | undefined,
): string | undefined {
  if (!projectId) return undefined;
  const ws = state.workspaces.find((w) =>
    w.members.some((m) => m.projectId === projectId),
  );
  return ws?.id ?? projectId;
}

/** Does a thread warrant a nudge? An unseen attention moment (finished turn,
 *  error) or a live pending gate. `waiting_approval` is included even when
 *  unwatched-and-unflagged so it re-badges from the persisted status after a
 *  reload. */
export function threadNeedsAttention(state: AppState, thread: Thread): boolean {
  return !!state.attention[thread.id] || thread.status === "waiting_approval";
}

/** What a whole project is doing, for the sidebar's project-level indicator.
 *
 *  `attention` outranks `working` deliberately, and it is the same precedence
 *  the rows use: a chat that wants a human beats one that is merely busy. A
 *  project with both should pull you toward the one you can act on. */
export type ProjectActivity = "attention" | "working" | null;

export function projectActivity(
  state: AppState,
  threads: readonly Thread[],
): ProjectActivity {
  let working = false;
  for (const thread of threads) {
    if (threadNeedsAttention(state, thread)) return "attention";
    if (thread.status === "running") working = true;
  }
  return working ? "working" : null;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** Reconcile this device's unread markers against server-side settling.
 *
 *  Unread lives in localStorage (it is per-person), settling lives on the
 *  server (it is per-chat). Without this they contradict each other across
 *  devices: settle a finished chat on your phone and the desktop still holds
 *  its own `attention` flag, which outranks settled — so the shelf you just
 *  tidied on one device stays untidy on the other, forever, because only
 *  opening the chat clears the flag. Observing `settledAt` is that
 *  acknowledgement arriving. */
function dropAttentionForSettled(
  attention: Record<string, true>,
  threads: readonly Thread[],
): Record<string, true> {
  let next: Record<string, true> | null = null;
  for (const t of threads) {
    if (t.settledAt && attention[t.id]) {
      next ??= { ...attention };
      delete next[t.id];
    }
  }
  return next ?? attention;
}

/** The slice of app state `threadSettled` reads. Narrower than `AppState` on
 *  purpose: callers memoize on these two fields instead of the whole store,
 *  which changes identity on every streamed token. `AppState` satisfies it
 *  structurally, so passing `state` still works. */
export interface SettleContext {
  attention: Record<string, true>;
  activeThreadId: string | null;
}

/** Is this chat parked in the settled shelf? Four rules, in order:
 *
 *  0. The chat you are READING is never parked. Auto-settle would otherwise
 *     swallow the row the moment you opened it — opening clears the unread
 *     marker, and a quiet-but-unread chat is exactly the kind that is past
 *     the idle window — leaving you reading a chat the sidebar says
 *     doesn't exist.
 *  1. Work that needs a human is NEVER parked. A live turn, a pending
 *     approval/question, or an unread finish outranks any settle — including
 *     an explicit one. The server un-settles on real activity too, but the
 *     client can see the new status a beat before that broadcast lands, and
 *     a chat asking for you must never be hidden in the meantime.
 *  2. An explicit settle parks it, full stop.
 *  3. Otherwise it settles itself once it has been quiet for `autoSettleDays`
 *     (null disables that entirely). Pulling a chat back out of the shelf
 *     restarts that clock rather than pinning it, so reading an old chat
 *     buys it a fresh window instead of exempting it forever.
 *
 *  `now` is passed in so callers can quantize it and keep the result stable
 *  across renders instead of recomputing against a moving clock. */
export function threadSettled(
  ctx: SettleContext,
  thread: Thread,
  autoSettleDays: number | null,
  now: number,
): boolean {
  if (ctx.activeThreadId === thread.id) return false;
  if (thread.status === "running" || thread.status === "waiting_approval") return false;
  if (ctx.attention[thread.id]) return false;
  if (thread.settledAt) return true;
  if (autoSettleDays === null) return false;
  const lastActivity = Math.max(
    Date.parse(thread.updatedAt),
    thread.keptActiveAt ? Date.parse(thread.keptActiveAt) : Number.NEGATIVE_INFINITY,
  );
  // A malformed timestamp must never park a chat by accident.
  if (!Number.isFinite(lastActivity)) return false;
  return lastActivity < now - autoSettleDays * DAY_MS;
}

/** Hermes chats currently asking to be looked at, most recently active first.
 *  Drives both the fleet-view attention dot and which chat the agents button
 *  jumps to. */
export function hermesAttentionThreads(state: AppState): Thread[] {
  return Object.values(state.threads)
    .flat()
    .filter((t) => t.agent === "hermes" && threadNeedsAttention(state, t))
    .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
}

/** The machineId to attach to a routed RPC: the owner's id when it is NOT
 *  this machine, undefined for local (so local calls stay byte-identical). */
export function remoteMachineId(
  state: AppState,
  machineId: string | undefined,
): string | undefined {
  if (!machineId) return undefined;
  const local = state.hello?.machineId;
  return local && machineId !== local ? machineId : undefined;
}

/** Which machine owns a project, resolved from workspace membership. */
export function projectOwner(state: AppState, projectId: string): string | undefined {
  for (const w of state.workspaces) {
    const m = w.members.find((x) => x.projectId === projectId);
    if (m) return m.machineId;
  }
  return undefined;
}

/** Resolve a projectId to something the workspace panes can render: the
 *  local Project record, or — for a root on another machine — a project
 *  view synthesized from the workspace-member snapshot plus the owning
 *  machineId (all pane traffic then routes/streams through the peer). */
export function resolveProjectView(
  state: AppState,
  projectId: string | null | undefined,
): { project: Project; machineId?: string } | null {
  if (!projectId) return null;
  const local = state.projects.find((p) => p.id === projectId);
  if (local) return { project: local };
  for (const w of state.workspaces) {
    const m = w.members.find((x) => x.projectId === projectId);
    if (m) {
      const machineId = remoteMachineId(state, m.machineId);
      if (!machineId) return null; // claims local but no record — deleted
      return {
        project: {
          id: m.projectId,
          name: m.name ?? "remote folder",
          path: m.path ?? "",
          createdAt: w.createdAt,
        },
        machineId,
      };
    }
  }
  return null;
}

export function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "conn":
      // A (re)connect begins fresh presence revisioning: the server numbers
      // revisions per-process, so drop our high-water mark now, before the
      // reconnect re-requests the snapshot, so the new process's frames apply.
      if (action.conn === "connecting")
        return { ...state, conn: action.conn, hermesRevision: -1 };
      return { ...state, conn: action.conn };
    case "isTauri":
      return { ...state, isTauri: action.value };
    case "solo":
      return { ...state, solo: action.projectId };
    case "dragProject":
      return { ...state, dragProject: action.project };
    case "http":
      return { ...state, http: action.value };
    case "hello":
      return { ...state, hello: action.data };
    case "attentionSync":
      return { ...state, attention: action.attention };
    case "attentionClear": {
      // Parking a chat is an acknowledgement: it must not land in the shelf
      // still wearing an unread dot (which would immediately pull it back
      // out, since unread outranks settled).
      if (!state.attention[action.threadId]) return state;
      const attention = { ...state.attention };
      delete attention[action.threadId];
      return { ...state, attention };
    }
    case "projects": {
      // Drop thread lists for removed projects.
      const keep = new Set(action.projects.map((p) => p.id));
      const threads: Record<string, Thread[]> = {};
      for (const pid of Object.keys(state.threads)) {
        if (keep.has(pid) || isQuickHomeProjectId(pid)) threads[pid] = state.threads[pid];
      }
      return { ...state, projects: action.projects, threads };
    }
    case "workspaces":
      return { ...state, workspaces: action.workspaces };
    case "peers":
      return { ...state, peers: action.peers, discovered: action.discovered };
    case "hermesAgents":
      return { ...state, hermesAgents: action.agents };
    case "customThemes":
      // The first list (even an empty one) marks the records as seeded, so
      // ThemeSync can distinguish "not loaded yet" from "genuinely deleted".
      return { ...state, customThemes: action.themes, themesLoaded: true };
    case "hermesStatuses":
      // Drop a stale frame/snapshot that lost a delivery race with a fresher
      // one (equal revision is identical content — a harmless no-op to skip).
      if (action.revision <= state.hermesRevision) return state;
      return {
        ...state,
        hermesRevision: action.revision,
        hermesStatuses: Object.fromEntries(action.statuses.map((s) => [s.agentId, s])),
      };
    case "threads":
      return {
        ...state,
        threads: { ...state.threads, [action.projectId]: sortThreads(action.threads) },
        attention: dropAttentionForSettled(state.attention, action.threads),
      };
    case "toggleProject":
      return {
        ...state,
        collapsed: {
          ...state.collapsed,
          [action.projectId]: !state.collapsed[action.projectId],
        },
      };
    case "openThread": {
      // Opening a thread clears its unseen-attention flag.
      let attention = state.attention;
      if (attention[action.threadId]) {
        attention = { ...attention };
        delete attention[action.threadId];
      }
      return {
        ...state,
        activeThreadId: action.threadId,
        draft: null,
        feed: [],
        feedThreadId: null,
        feedLoading: true,
        lastSeq: 0,
        sidebarOpen: false,
        attention,
      };
    }
    case "openDraft":
      return {
        ...state,
        activeThreadId: null,
        draft: action.draft,
        feed: [],
        feedThreadId: null,
        feedLoading: false,
        lastSeq: 0,
        sidebarOpen: false,
      };
    case "closeActive":
      return {
        ...state,
        activeThreadId: null,
        draft: null,
        feed: [],
        feedThreadId: null,
        feedLoading: false,
        lastSeq: 0,
      };
    case "feedLoaded": {
      if (state.activeThreadId !== action.threadId) return state;
      let feed: FeedItem[] = [];
      let lastSeq = 0;
      for (const pe of action.events) {
        feed = applyEvent(feed, pe.event, pe.ts, pe.speaker);
        if (pe.seq > lastSeq) lastSeq = pe.seq;
      }
      const threads = patchThread(state.threads, action.threadId, () => action.thread);
      return { ...state, feed, feedThreadId: action.threadId, feedLoading: false, lastSeq, threads };
    }
    case "agentEvent": {
      const { threadId, seq, timestamp, speaker, event } = action;
      let next = state;

      const status = statusFromEvent(event);
      const threads = patchThread(state.threads, threadId, (t) => ({
        ...t,
        status: status ?? t.status,
        updatedAt: seq > 0 ? timestamp : t.updatedAt,
      }));
      if (threads !== state.threads) next = { ...next, threads };

      const viewing = state.feedThreadId === threadId && state.activeThreadId === threadId;
      if (viewing) {
        // Drop persisted duplicates during replay races; deltas (seq -1) always apply.
        if (seq > 0 && seq <= state.lastSeq) return next;
        const feed = applyEvent(
          next === state ? state.feed : next.feed,
          event,
          timestamp,
          speaker,
        );
        next = {
          ...next,
          feed,
          lastSeq: seq > 0 ? seq : next.lastSeq,
        };
      } else if (seq >= 0 && isAttentionEvent(event) && !state.attention[threadId]) {
        // A moment worth attention landed on a thread the user isn't watching.
        // (seq < 0 are optimistic local echoes, never a reason to self-badge.)
        next = { ...next, attention: { ...next.attention, [threadId]: true } };
      }
      return next;
    }
    case "threadUpserted": {
      const t = action.thread;
      const list = state.threads[t.projectId] ?? [];
      const idx = list.findIndex((x) => x.id === t.id);
      const nextList = idx >= 0 ? list.map((x) => (x.id === t.id ? t : x)) : [...list, t];
      return {
        ...state,
        threads: { ...state.threads, [t.projectId]: sortThreads(nextList) },
        attention: dropAttentionForSettled(state.attention, [t]),
      };
    }
    case "threadDeleted": {
      const list = (state.threads[action.projectId] ?? []).filter(
        (t) => t.id !== action.threadId,
      );
      const cleared = state.activeThreadId === action.threadId;
      let attention = state.attention;
      if (attention[action.threadId]) {
        attention = { ...attention };
        delete attention[action.threadId];
      }
      return {
        ...state,
        threads: { ...state.threads, [action.projectId]: list },
        activeThreadId: cleared ? null : state.activeThreadId,
        feed: cleared ? [] : state.feed,
        feedThreadId: cleared ? null : state.feedThreadId,
        feedLoading: cleared ? false : state.feedLoading,
        attention,
      };
    }
    case "projectDeleted": {
      const projects = state.projects.filter((p) => p.id !== action.projectId);
      // Mirror the server: drop the membership, and the workspace itself
      // once no members remain.
      const workspaces = state.workspaces
        .map((w) => ({
          ...w,
          members: w.members.filter((m) => m.projectId !== action.projectId),
        }))
        .filter((w) => w.members.length > 0);
      const threads = { ...state.threads };
      const gone = threads[action.projectId] ?? [];
      delete threads[action.projectId];
      const terminals = { ...state.terminals };
      delete terminals[action.projectId];
      const artifacts = { ...state.artifacts };
      delete artifacts[action.projectId];
      const git = { ...state.git };
      delete git[action.projectId];
      const workspace = { ...state.workspace };
      delete workspace[action.projectId];
      const activeGone =
        (state.activeThreadId && gone.some((t) => t.id === state.activeThreadId)) ||
        state.draft?.projectId === action.projectId;
      // Drop attention flags for the project's now-gone threads.
      let attention = state.attention;
      if (gone.some((t) => attention[t.id])) {
        attention = { ...attention };
        for (const t of gone) delete attention[t.id];
      }
      return {
        ...state,
        projects,
        workspaces,
        threads,
        terminals,
        artifacts,
        git,
        workspace,
        activeThreadId: activeGone ? null : state.activeThreadId,
        draft: activeGone ? null : state.draft,
        feed: activeGone ? [] : state.feed,
        feedThreadId: activeGone ? null : state.feedThreadId,
        attention,
      };
    }
    case "draftSettings":
      if (!state.draft) return state;
      return {
        ...state,
        draft: { ...state.draft, agent: action.agent, settings: action.settings },
      };
    case "approvalPending": {
      const feed = state.feed.map((it) =>
        it.type === "approval" && it.approvalId === action.approvalId
          ? { ...it, pending: true }
          : it,
      );
      return { ...state, feed };
    }
    case "questionAnswer": {
      const feed = state.feed.map((it) =>
        it.type === "question" && it.requestId === action.requestId && !it.answered
          ? { ...it, answers: action.answers }
          : it,
      );
      return { ...state, feed };
    }
    case "questionPending": {
      // Optimistically collapse to the answered summary on submit.
      const feed = state.feed.map((it) =>
        it.type === "question" && it.requestId === action.requestId
          ? { ...it, pending: true, answered: true }
          : it,
      );
      return { ...state, feed };
    }
    case "sidebar":
      return { ...state, sidebarOpen: action.open };
    case "usage":
      return { ...state, usage: action.usage };
    case "noticeAdd":
      // Newest last; cap the stack so an unattended session can't flood it.
      return { ...state, notices: [...state.notices.slice(-3), action.notice] };
    case "noticeDismiss":
      return { ...state, notices: state.notices.filter((n) => n.id !== action.id) };
    case "workspace": {
      const workspace = { ...state.workspace };
      if (action.tab == null) delete workspace[action.projectId];
      else workspace[action.projectId] = action.tab;
      return { ...state, workspace };
    }
    case "schedules":
      return { ...state, schedules: action.schedules };
    case "terminals":
      return {
        ...state,
        terminals: { ...state.terminals, [action.projectId]: action.terminals },
      };
    case "artifacts":
      return {
        ...state,
        artifacts: { ...state.artifacts, [action.projectId]: action.artifacts },
      };
    case "artifactFocus":
      return { ...state, artifactFocus: action.artifactId };
    case "git":
      return {
        ...state,
        git: { ...state.git, [action.projectId]: action.repos },
      };
    case "archives":
      return {
        ...state,
        archives: { ...state.archives, [action.machineId]: action.archives },
      };
    case "update":
      return { ...state, update: action.update };
    default:
      return state;
  }
}

// ---- context -------------------------------------------------------------

export interface ThreadknotActions {
  /** All four take the machine whose logins are being managed; omitted means
   *  this one. */
  listBrowserProfiles: (
    machineId?: string,
  ) => Promise<import("../lib/protocol").BrowserProfileInfo[]>;
  createBrowserProfile: (
    name: string,
    origins: string[],
    machineId?: string,
  ) => Promise<import("../lib/protocol").BrowserProfileInfo>;
  updateBrowserProfile: (
    profileId: string,
    patch: { name?: string; origins?: string[] },
    machineId?: string,
  ) => Promise<import("../lib/protocol").BrowserProfileInfo>;
  deleteBrowserProfile: (profileId: string, machineId?: string) => Promise<void>;
  listMobileDevices: () => Promise<import("../lib/protocol").MobileDeviceInfo[]>;
  revokeMobileDevice: (deviceId: string) => Promise<void>;
  /** Replace a paired device's grants. Narrowing them also closes whatever
   *  sockets that device currently holds. */
  setMobileDeviceCapabilities: (
    deviceId: string,
    capabilities: import("../lib/protocol").DeviceCapability[],
  ) => Promise<import("../lib/protocol").MobileDeviceInfo>;
  /** Mint a one-time pairing code + QR for the phone to scan, bound to the
   *  grants the owner picked and to the address it should point at. */
  beginMobilePairing: (
    capabilities?: import("../lib/protocol").DeviceCapability[],
    target?: import("../lib/protocol").PairingTarget,
  ) => Promise<import("../lib/protocol").PairingQr>;
  /** The hosted relay connector for THIS machine. Master-only on the server. */
  getConnectorStatus: () => Promise<import("../lib/protocol").ConnectorStatus>;
  /** Register with the relay using a token copied from the console. The
   *  hostname comes back from the server; this machine never proposes one. */
  enrollConnector: (
    enrollmentToken: string,
    machineName?: string,
  ) => Promise<import("../lib/protocol").ConnectorStatus>;
  /** Ask the relay to open a connection request and start watching for the
   *  answer. Nothing sensitive is displayed: the owner opens a URL and presses
   *  Approve, and the connector notices by itself. */
  beginConnectorApproval: (
    machineName?: string,
  ) => Promise<import("../lib/protocol").ConnectorApproval>;
  /** Stop watching. The request stays valid server-side until it expires. */
  cancelConnectorApproval: () => Promise<import("../lib/protocol").ConnectorStatus>;
  setConnectorEnabled: (
    enabled: boolean,
  ) => Promise<import("../lib/protocol").ConnectorStatus>;
  /** Remote access for THIS machine. Master-only on the server. */
  getRemoteAccess: () => Promise<import("../lib/protocol").RemoteAccess>;
  setRemoteAccess: (patch: {
    enabled?: boolean;
    origin?: string | null;
  }) => Promise<import("../lib/protocol").RemoteAccess>;
  /** Kill outstanding pairing codes the moment the QR is dismissed. */
  cancelMobilePairing: () => Promise<void>;
  listHermesAgents: () => Promise<import("../lib/protocol").HermesAgentInfo[]>;
  /** Probes the gateway before storing; rejects with a readable error. */
  addHermesAgent: (
    baseUrl: string,
    apiKey: string,
    name?: string,
  ) => Promise<import("../lib/protocol").HermesAgentInfo>;
  removeHermesAgent: (agentId: string) => Promise<void>;
  setHermesAgentImage: (agentId: string, image?: string) => Promise<import("../lib/protocol").HermesAgentInfo>;
  /** Set/clear an agent's profile photo (null clears). */
  setHermesAgentAvatar: (
    agentId: string,
    image: string | null,
  ) => Promise<import("../lib/protocol").HermesAgentInfo>;
  /** Live tools/skills/MCP snapshot fetched from the remote gateway. */
  hermesAgentDetails: (
    agentId: string,
  ) => Promise<import("../lib/protocol").HermesAgentDetails>;
  listClaudexProfiles: () => Promise<import("../lib/protocol").ClaudexProfileInfo[]>;
  addClaudexProfile: (
    input: import("../lib/protocol").ClaudexProfileInput,
  ) => Promise<import("../lib/protocol").ClaudexProfileInfo>;
  /** Omitted fields keep their stored value — including `authToken`. */
  updateClaudexProfile: (
    profileId: string,
    input: import("../lib/protocol").ClaudexProfileInput,
  ) => Promise<import("../lib/protocol").ClaudexProfileInfo>;
  removeClaudexProfile: (profileId: string) => Promise<void>;
  setClaudexProfileAvatar: (
    profileId: string,
    image: string | null,
  ) => Promise<import("../lib/protocol").ClaudexProfileInfo>;
  /** Reachability check; starts the profile's sidecar if it has one. */
  testClaudexProfile: (profileId: string) => Promise<{
    sidecar: import("../lib/protocol").ClaudexSidecarState;
    baseUrl: string;
  }>;
  /** Reload the machine's custom themes into `state.customThemes`. */
  listThemes: () => Promise<CustomTheme[]>;
  /** Upsert a theme; the server stamps its timestamps and returns it. */
  saveTheme: (theme: CustomTheme) => Promise<CustomTheme>;
  removeTheme: (themeId: string) => Promise<void>;
  /** Ask the local Claude CLI to design a palette from a wallpaper data URL. */
  aiPalette: (imageDataUrl: string, hint?: string) => Promise<AiPalette>;
  refreshProjects: () => Promise<void>;
  /** Reload the sidebar's workspace containers into `state.workspaces`. */
  refreshWorkspaces: () => Promise<void>;
  renameWorkspace: (workspaceId: string, name: string) => Promise<void>;
  setWorkspaceFavorite(workspaceId: string, favorite: boolean): Promise<void>;
  /** Stash a workspace out of the sidebar, or bring it back. Nothing is
   *  deleted: its roots, chats and running agents are untouched. */
  setWorkspaceHidden(workspaceId: string, hidden: boolean): Promise<void>;
  setWorkspaceImage: (workspaceId: string, image?: string) => Promise<void>;
  /** Reload paired peers + LAN-discovered machines into state. */
  refreshPeers: () => Promise<void>;
  /** Attach a folder on `machineId` as a new root of the workspace; resolves
   *  with the (created or reused) project on that machine. */
  attachRoot: (workspaceId: string, machineId: string, path: string) => Promise<Project>;
  /** Detach a root (the founding root can't be detached). */
  detachRoot: (workspaceId: string, machineId: string, projectId: string) => Promise<void>;
  /** Pair with another Threadknot: URL (may embed ?token=…) + optional token. */
  addPeer: (url: string, token?: string) => Promise<void>;
  removePeer: (machineId: string) => Promise<void>;
  /** Rename THIS machine (the name peers see); refreshes hello. */
  renameDevice: (name: string) => Promise<void>;
  /** Set THIS machine's avatar/color (omitted field = unchanged, null = clear). */
  setDeviceAppearance: (patch: { image?: string | null; color?: string | null }) => Promise<void>;
  /** Local override for how a PEER looks on this machine (this machine only). */
  setPeerAppearance: (
    machineId: string,
    patch: { image?: string | null; color?: string | null },
  ) => Promise<void>;
  /** Edit a PEER's REAL profile everywhere via a routed mesh command: name via
   *  device.rename and/or avatar/color via device.setAppearance, both carrying
   *  the target machineId so they run on (and gossip from) that machine. */
  setPeerProfile: (
    machineId: string,
    patch: { name?: string; image?: string | null; color?: string | null },
  ) => Promise<void>;
  refreshThreads: (projectId: string, machineId?: string) => Promise<void>;
  /** Re-pull remote workspace members' threads (e.g. after a peer reconnects). */
  refreshRemoteThreads: () => Promise<void>;
  /** Open a thread normally, or refresh its transcript in place while keeping
   *  mounted chat UI (scroll position, open HUD/popovers) intact. */
  selectThread: (
    threadId: string,
    options?: { preserveFeed?: boolean },
  ) => Promise<void>;
  /** Full text of a historical tool call whose replay copy was elided. */
  toolOutput: (threadId: string, callId: string) => Promise<string | null>;
  openDraft: (projectId: string, machineId?: string) => void;
  /** Start a folderless chat in an isolated scratch directory on a machine. */
  openQuickDraft: (machineId?: string) => void;
  /** Start a chat with one Hermes gateway: a draft in the hidden Hermes home
   *  project, locked to agent "hermes" with `model` = the registry agent id. */
  openHermesDraft: (hermesAgentId: string) => void;
  send: (text: string, attachments?: import("../lib/protocol").OutgoingAttachment[]) => Promise<void>;
  /** Inject or queue extra context without interrupting the running work. */
  steer: (text: string) => Promise<void>;
  interrupt: () => Promise<void>;
  respondApproval: (approvalId: string, optionId: string) => Promise<void>;
  setQuestionAnswers: (requestId: string, answers: Record<string, string[]>) => void;
  respondQuestion: (requestId: string, answers: Record<string, string[]>) => Promise<void>;
  setSettings: (settings: ThreadSettings) => Promise<void>;
  setDraftAgent: (agent: Agent) => void;
  /** Mid-thread provider switch: next turn runs on `agent`, context carried over. */
  setThreadAgent: (agent: Agent) => Promise<void>;
  /** Throw an adversarial reviewer at the active thread for one turn
   *  (Parley). The reviewer reads the real transcript via the handoff seed and
   *  yields the floor back to the builder when it's done. `settings.access`
   *  omitted means read-only. */
  reviewThread: (
    agent: Agent,
    settings: { model?: string; effort?: string; access?: Access },
    instructions?: string,
  ) => Promise<void>;
  /** Start a structured multi-agent debate (Parley): reviewers argue with the
   *  builder's work in rounds until they converge — then, with `execute`, the
   *  builder implements what was agreed. */
  startParley: (
    reviewers: {
      agent: Agent;
      model?: string;
      effort?: string;
      access?: Access;
      name?: string;
      personaId?: string;
      personality?: string;
    }[],
    opts: { rounds?: number; execute?: boolean; instructions?: string },
  ) => Promise<void>;
  /** Create/update a named reviewer persona (personas ride hello). */
  savePersona: (persona: ReviewerPersona) => Promise<void>;
  deletePersona: (personaId: string) => Promise<void>;
  /** The Library — installed skills, installed MCP servers, and the shipped
   *  catalog — for one machine (omitted = this one). */
  listLibrary: (
    machineId?: string,
  ) => Promise<import("../lib/protocol").LibraryData>;
  installSkill: (
    payload: {
      agents: import("../lib/protocol").SkillTarget[];
      catalogId?: string;
      source?: string;
      name?: string;
    },
    machineId?: string,
  ) => Promise<import("../lib/protocol").InstalledSkill>;
  /** Spread an already-installed skill into other agents' directories. */
  copySkill: (
    skillId: string,
    agents: import("../lib/protocol").SkillTarget[],
    machineId?: string,
  ) => Promise<void>;
  removeSkill: (
    skillId: string,
    agents: import("../lib/protocol").SkillTarget[],
    machineId?: string,
  ) => Promise<void>;
  saveMcpServer: (
    server: import("../lib/protocol").McpServerInfo,
    machineId?: string,
  ) => Promise<import("../lib/protocol").McpServerInfo>;
  installMcpServer: (
    payload: { catalogId: string; inputs?: Record<string, string>; name?: string },
    machineId?: string,
  ) => Promise<import("../lib/protocol").McpServerInfo>;
  deleteMcpServer: (serverId: string, machineId?: string) => Promise<void>;
  addProject: (path: string) => Promise<void>;
  deleteProject: (projectId: string) => Promise<void>;
  deleteThread: (threadId: string, projectId: string) => Promise<void>;
  renameThread: (threadId: string, title: string) => Promise<void>;
  /** Park a chat in the settled shelf, or pull it back out. Server-backed,
   *  so the shelf looks the same on every device. */
  setThreadSettled: (threadId: string, settled: boolean) => Promise<void>;
  setThreadFavorite(threadId: string, favorite: boolean): Promise<void>;
  /** One-shot log preview for the sidebar hover card. Routes to the owning
   *  machine automatically (like the other thread.* calls); never mutates
   *  store state - the caller caches the result itself. */
  threadPreview: (
    threadId: string,
  ) => Promise<import("../lib/protocol").ThreadPreview>;
  /** Search persisted transcript content across the supplied threads. Requests
   *  are grouped and routed to each thread's owning machine. */
  searchThreads: (query: string, threadIds: string[]) => Promise<string[]>;
  listDir: (
    path?: string,
    machineId?: string,
  ) => Promise<import("../lib/protocol").ListDirData>;
  refreshUsage: () => Promise<void>;
  /** The build's embedded update notes + git changelog, newest first
   *  (app.changelog). `notes` are client-facing; `entries` are internal. */
  fetchChangelog: () => Promise<{
    entries: import("../lib/protocol").ChangelogEntry[];
    notes: import("../lib/protocol").ReleaseNote[];
  }>;
  fsTree: (
    projectId: string,
    machineId?: string,
  ) => Promise<import("../lib/protocol").FileTreeData>;
  fsRead: (
    projectId: string,
    path: string,
    machineId?: string,
  ) => Promise<import("../lib/protocol").FileReadData>;
  /** Load a project's produced artifacts into `state.artifacts[projectId]`. */
  listArtifacts: (projectId: string) => Promise<ArtifactRecord[]>;
  /** Delete Threadknot's record and durable snapshot, leaving the project file intact. */
  deleteArtifact: (artifactId: string) => Promise<void>;
  scanPorts: () => Promise<{ ports: import("../lib/protocol").PortInfo[] }>;
  getDictationSettings: () => Promise<import("../lib/protocol").DictationSettings>;
  saveDictationSettings: (
    input: import("../lib/protocol").DictationSettingsInput,
  ) => Promise<import("../lib/protocol").DictationSettings>;
  /** Start recording this machine's mic; resolves to the id the other two take. */
  startDictation: () => Promise<string>;
  /** Stop and transcribe. Empty string means the clip held no speech. */
  stopDictation: (recordingId: string) => Promise<string>;
  /** Drop the clip without transcribing it. */
  cancelDictation: (recordingId: string) => Promise<void>;
  /** Rescan the project folder for repos; loads `state.git[projectId]`. */
  refreshGitRepos: (projectId: string) => Promise<import("../lib/protocol").GitRepoInfo[]>;
  gitStatus: (repoId: string) => Promise<import("../lib/protocol").GitStatusData>;
  gitDiff: (
    repoId: string,
    path: string,
    scope: import("../lib/protocol").GitDiffScope,
  ) => Promise<import("../lib/protocol").GitDiffData>;
  /** Mutations resolve with the repo's fresh status (server also broadcasts). */
  gitStage: (repoId: string, paths: string[]) => Promise<import("../lib/protocol").GitStatusData>;
  gitUnstage: (repoId: string, paths: string[]) => Promise<import("../lib/protocol").GitStatusData>;
  gitDiscard: (repoId: string, paths: string[]) => Promise<import("../lib/protocol").GitStatusData>;
  gitCommit: (
    repoId: string,
    message: string,
  ) => Promise<import("../lib/protocol").GitStatusData & { hash: string; subject: string }>;
  gitBranches: (repoId: string) => Promise<import("../lib/protocol").GitBranchesData>;
  gitCheckout: (
    repoId: string,
    branch: string,
    create?: boolean,
  ) => Promise<import("../lib/protocol").GitStatusData>;
  gitPush: (repoId: string) => Promise<import("../lib/protocol").GitStatusData & { output: string }>;
  gitPull: (repoId: string) => Promise<import("../lib/protocol").GitStatusData & { output: string }>;
  /** Commit several repos in one action; `link` stamps a shared Threadknot-Change trailer. */
  gitCommitMany: (
    payload: import("../lib/protocol").RequestMap["git.commitMany"]["payload"],
  ) => Promise<{ results: import("../lib/protocol").GitOpResult[]; changeId?: string | null }>;
  /** Create-or-switch the same branch across several repos. */
  gitCheckoutMany: (
    projectId: string,
    repoIds: string[],
    branch: string,
  ) => Promise<{ results: import("../lib/protocol").GitOpResult[] }>;
  /** Open a PR for the repo's current branch via the gh CLI. */
  gitPr: (
    repoId: string,
    title?: string,
    body?: string,
  ) => Promise<{ url?: string | null; output: string }>;
  /** Load this project's terminal tabs into `state.terminals[projectId]`. */
  refreshTerminals: (projectId: string) => Promise<void>;
  /** Create a new terminal tab; resolves with its record. */
  createTerminal: (projectId: string, name?: string) => Promise<import("../lib/protocol").TermInfo>;
  renameTerminal: (termId: string, name: string) => Promise<void>;
  /** Permanently delete a terminal tab (kills its shell + saved history). */
  deleteTerminal: (termId: string) => Promise<void>;
  refreshSchedules: () => Promise<void>;
  createSchedule: (
    payload: import("../lib/protocol").RequestMap["schedule.create"]["payload"],
  ) => Promise<Schedule>;
  updateSchedule: (
    payload: import("../lib/protocol").RequestMap["schedule.update"]["payload"],
  ) => Promise<Schedule>;
  deleteSchedule: (scheduleId: string) => Promise<void>;
  /** Fires the schedule immediately; resolves with the new thread's id. */
  runSchedule: (scheduleId: string) => Promise<string>;
  /** Export a thread to the archive dir and remove it from the live list. */
  archiveThread: (threadId: string, projectId: string) => Promise<void>;
  /** Reload one machine's archive list into `state.archives[machineId]`.
   *  Omit `machineId` (or pass the local id) for this machine; a remote id
   *  routes `archive.list` to that peer. */
  refreshArchives: (machineId?: string) => Promise<void>;
  /** Recreate an archived thread as a live thread on its owning machine, then
   *  navigate to it. `machineId` routes the restore to the owner (omit for
   *  local). */
  restoreArchive: (archiveId: string, machineId?: string) => Promise<void>;
  /** Permanently delete an archived session on its owning machine. `machineId`
   *  routes the delete to the owner (omit for local). */
  deleteArchive: (archiveId: string, machineId?: string) => Promise<void>;
  /** Current on-disk archive storage directory. */
  getArchiveDir: () => Promise<string>;
  /** Change the archive storage directory; resolves with the applied path. */
  setArchiveDir: (path: string) => Promise<string>;
  /** Load this machine's update status into `state.update`. */
  refreshUpdate: () => Promise<void>;
  /** Ask the server to re-check master now; the answer arrives via broadcast. */
  checkForUpdate: () => Promise<void>;
  /** Update status for any machine in the fleet (omit for this one). */
  updateStatusFor: (
    machineId?: string,
  ) => Promise<import("../lib/protocol").UpdateStatus>;
  /** Fast-forward a machine's checkout onto origin/master. Rejects when the
   *  machine carries local commits or uncommitted changes. */
  pullUpdate: (machineId?: string) => Promise<void>;
  /** Start a rebuild. Returns once the build is claimed, not once it is done:
   *  progress arrives on the `updates` broadcast. */
  rebuildUpdate: (machineId?: string) => Promise<void>;
  /** Relaunch a machine into the binary already built on its disk. `force`
   *  overrides the guard that protects threads mid-turn. */
  restartUpdate: (machineId?: string, force?: boolean) => Promise<void>;
  /** Point a machine at its Threadknot source checkout (machine-local setting). */
  setUpdateRepoPath: (path: string, machineId?: string) => Promise<void>;
}

export interface StoreValue {
  state: AppState;
  dispatch: React.Dispatch<Action>;
  actions: ThreadknotActions;
}

export const StoreContext = createContext<StoreValue | null>(null);

export function useStore(): StoreValue {
  const ctx = useContext(StoreContext);
  if (!ctx) throw new Error("useStore outside provider");
  return ctx;
}

/**
 * Pick a valid effort override for a model. Claude's preferred default is the
 * absence of an override: the CLI then applies Anthropic's model-specific
 * default without receiving an --effort flag.
 */
export function effortForModel(
  model: AgentModel | undefined,
  current?: string,
  preferProviderDefault = false,
): string | undefined {
  const efforts = model?.efforts;
  if (!efforts || efforts.length === 0) return undefined;
  if (current && efforts.includes(current)) return current;
  if (preferProviderDefault) return undefined;
  if (model!.defaultEffort && efforts.includes(model!.defaultEffort)) return model!.defaultEffort;
  return efforts.includes("medium") ? "medium" : efforts[0];
}

/** Remember the last controls the user chose for the next chat in this project. */
export function rememberNewThreadSettings(
  projectId: string,
  agent: Agent,
  settings: ThreadSettings,
): void {
  try {
    localStorage.setItem(
      newThreadSettingsKey(projectId),
      JSON.stringify({ agent, settings, defaultsVersion: NEW_THREAD_DEFAULTS_VERSION }),
    );
  } catch {
    // Preferences are a convenience; private/locked-down storage must not block chat.
  }
}

export function forgetNewThreadSettings(projectId: string): void {
  try {
    localStorage.removeItem(newThreadSettingsKey(projectId));
  } catch {
    // See rememberNewThreadSettings.
  }
}

function storedNewThreadSettings(
  projectId: string,
): { agent: Agent; settings: unknown; defaultsVersion?: number } | null {
  try {
    const raw = localStorage.getItem(newThreadSettingsKey(projectId));
    if (!raw) return null;
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object") return null;
    const agent = (value as { agent?: unknown }).agent;
    const settings = (value as { settings?: unknown }).settings;
    const defaultsVersion = (value as { defaultsVersion?: unknown }).defaultsVersion;
    if (
      (agent !== "claude" &&
        agent !== "codex" &&
        agent !== "kimi" &&
        agent !== "hermes" &&
        agent !== "claudex") ||
      !settings ||
      typeof settings !== "object"
    ) {
      return null;
    }
    return {
      agent,
      settings,
      defaultsVersion: typeof defaultsVersion === "number" ? defaultsVersion : undefined,
    };
  } catch {
    return null;
  }
}

export function defaultDraft(
  state: AppState,
  projectId: string,
  machineId?: string,
): DraftThread {
  const agents = (state.hello?.agents ?? []).filter((a) => isAgentVisible(a.id));
  const stored = storedNewThreadSettings(projectId);
  const active = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
  const recent =
    active?.projectId === projectId ? active : (state.threads[projectId]?.[0] ?? null);
  // On the first run after this feature lands, seed from the project's current/
  // most-recent session. Once the user changes a control, the explicit project
  // preference above wins and merely opening an older chat cannot overwrite it.
  const remembered =
    stored ?? (recent ? { agent: recent.agent, settings: recent.settings } : null);
  const preferred =
    agents.find((a) => a.id === remembered?.agent) ??
    agents.find((a) => a.available) ??
    agents[0];
  const agent: Agent = preferred?.id ?? "claude";
  const storedSettings = remembered?.settings as Partial<ThreadSettings> | undefined;
  // Preserve a user's per-project Opus preference across the model replacement.
  // The server performs the same migration for persisted threads/schedules.
  const rememberedModel =
    preferred?.id === "claude" && storedSettings?.model === RETIRED_CLAUDE_OPUS_MODEL
      ? CURRENT_CLAUDE_OPUS_MODEL
      : storedSettings?.model;
  const storedModel =
    typeof rememberedModel === "string" &&
    preferred?.models.some((m) => m.id === rememberedModel)
      ? rememberedModel
      : undefined;
  const modelId = storedModel ?? preferred?.defaultModel ?? preferred?.models[0]?.id ?? "";
  const model = preferred?.models.find((m) => m.id === modelId);
  // Existing installations remembered Sol's old provider default ("low").
  // Upgrade that one legacy default to Threadknot's new high default once; choices
  // explicitly saved after this change continue to be remembered as usual.
  const migratedCodexEffort =
    remembered?.agent === "codex" &&
    modelId === "gpt-5.6-sol" &&
    remembered.defaultsVersion !== NEW_THREAD_DEFAULTS_VERSION &&
    storedSettings?.effort === "low"
      ? undefined
      : storedSettings?.effort;
  // Claude used to persist Anthropic's advertised default as a concrete
  // override. Migrate that indistinguishable old default once so existing
  // project drafts adopt the new flag-free Default choice.
  const rememberedEffort =
    remembered?.agent === "claude" &&
    remembered.defaultsVersion !== NEW_THREAD_DEFAULTS_VERSION &&
    migratedCodexEffort === model?.defaultEffort
      ? undefined
      : migratedCodexEffort;
  const access =
    storedSettings?.access === "read" ||
    storedSettings?.access === "edits" ||
    storedSettings?.access === "full"
      ? storedSettings.access
      : "full";
  const mode =
    storedSettings?.mode === "plan" || storedSettings?.mode === "build"
      ? storedSettings.mode
      : "build";
  return {
    projectId,
    machineId: machineId ?? projectOwner(state, projectId) ?? state.hello?.machineId ?? "",
    agent,
    settings: {
      model: modelId,
      effort: effortForModel(
        model,
        typeof rememberedEffort === "string" ? rememberedEffort : undefined,
        agent === "claude",
      ),
      wideContext:
        agent === "claude" && model?.supportsWideContext && storedSettings?.wideContext === true
          ? true
          : undefined,
      claudeChrome:
        agent === "claude" && storedSettings?.claudeChrome === true ? true : undefined,
      access,
      mode,
    },
  };
}
