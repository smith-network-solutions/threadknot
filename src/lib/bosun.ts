// Pure helpers for the Bosun UI (docs/BOSUN.md §8): the sidebar timeline, the
// wake summary line, the create-form draft and its validation. Nothing here
// touches the DOM or the socket, so scripts/test-bosun-ui.mjs can import it
// straight through esbuild.

import type {
  Access,
  Agent,
  Bosun,
  BosunTask,
  Cadence,
  Charter,
  DecisionKind,
  Lookout,
  LookoutKind,
  LookoutType,
  PersistedEvent,
  Question,
  QuietHours,
  TaskEvent,
  TaskStatus,
  Thread,
  ThreadSettings,
  TriageSettings,
  Wake,
  WakeBudget,
  WorkDefaults,
  Workspace,
} from "./protocol";
import { nextOccurrence } from "./schedule";
import { threadNeedsAttention, type AppState } from "../state/store";

// ---- threads -------------------------------------------------------------

export function isBosunThread(thread: Thread): boolean {
  return thread.origin?.kind === "bosun";
}

/** Anchors (with or without the emoji variation selector) and whitespace at
 *  the start of a title. The engine prefixes bosun thread titles with "⚓ ",
 *  and merged follow-ups can stack several. */
const LEADING_MARKS = /^[\s\u2693\uFE0F]+/u;

/** A title as it should be shown: any leading run of anchors and spaces
 *  removed. The mark is drawn once, as an icon, by whatever renders the row;
 *  it never belongs in the text. Pure — the stored title is untouched. */
export function stripTitleMarks(title: string | null | undefined): string {
  return (title ?? "").replace(LEADING_MARKS, "").trim();
}

/** The display title of a thread (empty string when it has none, so callers
 *  keep their own "Untitled thread" fallback). */
export function displayTitle(thread: Pick<Thread, "title"> | null | undefined): string {
  return stripTitleMarks(thread?.title);
}

/** Every thread a Bosun opened, newest first. */
export function bosunThreads(state: Pick<AppState, "threads">): Thread[] {
  return Object.values(state.threads)
    .flat()
    .filter(isBosunThread)
    .sort((a, b) => (a.createdAt < b.createdAt ? 1 : a.createdAt > b.createdAt ? -1 : 0));
}

/** Bosun threads currently asking to be looked at, most recently active
 *  first. Drives the launch button's dot, like `hermesAttentionThreads`. */
export function bosunAttentionThreads(state: AppState): Thread[] {
  return Object.values(state.threads)
    .flat()
    .filter((t) => isBosunThread(t) && threadNeedsAttention(state, t))
    .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
}

// ---- timeline ------------------------------------------------------------

const pad2 = (n: number) => n.toString().padStart(2, "0");

/** Local calendar day of an ISO timestamp, as "YYYY-MM-DD". */
export function localDayKey(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "unknown";
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/** Local "HH:MM" (24 h) of an ISO timestamp. */
export function localTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "--:--";
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/** "Today" / "Yesterday" / "Mon, Sep 28". */
export function dayLabel(key: string, now: Date = new Date()): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(key);
  if (!m) return key;
  const today = localDayKey(now.toISOString());
  if (key === today) return "Today";
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (key === localDayKey(y.toISOString())) return "Yesterday";
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  const label = `${WEEKDAYS[d.getDay()]}, ${MONTHS[d.getMonth()]} ${d.getDate()}`;
  return d.getFullYear() === now.getFullYear() ? label : `${label} ${d.getFullYear()}`;
}

export interface DaySummary {
  wakes: number;
  signals: number;
  ignored: number;
  logged: number;
  /** work + ask: the wakes' decisions that put someone to work. */
  woke: number;
  /** Wakes that failed or hit the budget (`skipped` set), newest first. */
  skipped: Wake[];
}

export interface TimelineDay {
  key: string;
  /** The day's wakes, newest first. Shown only when the summary is expanded. */
  wakes: Wake[];
  /** Threads a bosun opened that day, newest first. The day's content. */
  threads: Thread[];
  summary: DaySummary;
}

const isoMs = (iso: string) => {
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : 0;
};

/** Totals across a set of wakes, for the one summary line per day. */
export function summarizeWakes(wakes: readonly Wake[]): DaySummary {
  const s: DaySummary = { wakes: 0, signals: 0, ignored: 0, logged: 0, woke: 0, skipped: [] };
  for (const w of wakes) {
    const c = decisionCounts(w);
    s.wakes += 1;
    s.signals += w.signals;
    s.ignored += c.ignore;
    s.logged += c.log;
    s.woke += c.work + c.ask;
    if (w.skipped) s.skipped.push(w);
  }
  s.skipped.sort((a, b) => isoMs(b.at) - isoMs(a.at));
  return s;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** `Today · 9 wakes · 14 signals · ignored 5 · logged 6 · woke 3`. A day with
 *  no wakes is just its label. */
export function daySummaryLine(label: string, s: DaySummary): string {
  if (s.wakes === 0) return label;
  return [
    label,
    plural(s.wakes, "wake"),
    plural(s.signals, "signal"),
    `ignored ${s.ignored}`,
    `logged ${s.logged}`,
    `woke ${s.woke}`,
  ].join(" · ");
}

/** `2 wakes skipped: triage failed: timeout` — the newest reason, plus a
 *  count of other distinct reasons. Null when nothing was skipped. */
export function skippedLine(s: DaySummary): string | null {
  if (s.skipped.length === 0) return null;
  const reasons = [...new Set(s.skipped.map((w) => w.skipped!.trim()))];
  const more = reasons.length > 1 ? ` (+${reasons.length - 1} other reason${reasons.length === 2 ? "" : "s"})` : "";
  return `${plural(s.skipped.length, "wake")} skipped: ${reasons[0]}${more}`;
}

/** Wakes and the threads bosuns opened, grouped by local day (newest day
 *  first). Threads are the content; wakes are folded into one summary per
 *  day. A thread is placed by `createdAt`: the day its wake opened it. */
export function bosunTimeline(wakes: readonly Wake[], threads: readonly Thread[]): TimelineDay[] {
  const byDay = new Map<string, { wakes: Wake[]; threads: Thread[] }>();
  const day = (iso: string) => {
    const key = localDayKey(iso);
    let d = byDay.get(key);
    if (!d) {
      d = { wakes: [], threads: [] };
      byDay.set(key, d);
    }
    return d;
  };
  for (const wake of wakes) day(wake.at).wakes.push(wake);
  for (const thread of threads) {
    if (isBosunThread(thread)) day(thread.createdAt).threads.push(thread);
  }
  return [...byDay.entries()]
    .sort(([a], [b]) => (a < b ? 1 : a > b ? -1 : 0))
    .map(([key, d]) => {
      const ws = d.wakes.sort((a, b) => isoMs(b.at) - isoMs(a.at));
      return {
        key,
        wakes: ws,
        threads: d.threads.sort((a, b) => isoMs(b.createdAt) - isoMs(a.createdAt)),
        summary: summarizeWakes(ws),
      };
    });
}

export function decisionCounts(wake: Wake): Record<DecisionKind, number> {
  const counts: Record<DecisionKind, number> = { ignore: 0, log: 0, work: 0, ask: 0 };
  for (const d of wake.decisions) {
    if (d.decision in counts) counts[d.decision] += 1;
  }
  return counts;
}

/** `16:02 · 3 signals · ignored 2, logged 1, woke 0` — "woke" is work+ask.
 *  A skipped wake says why instead of pretending it decided anything. */
export function wakeSummary(wake: Wake): string {
  const c = decisionCounts(wake);
  const head = `${localTime(wake.at)} · ${wake.signals} signal${wake.signals === 1 ? "" : "s"}`;
  const tally = `ignored ${c.ignore}, logged ${c.log}, woke ${c.work + c.ask}`;
  if (wake.skipped && wake.decisions.length === 0) return `${head} · ${wake.skipped}`;
  return wake.skipped ? `${head} · ${tally} · ${wake.skipped}` : `${head} · ${tally}`;
}

// ---- draft + validation --------------------------------------------------

/** The editable part of a Bosun: what the form holds and what save sends. */
export interface BosunDraft {
  /** Set when editing an existing bosun. */
  id?: string;
  name: string;
  image: string | null;
  enabled: boolean;
  homeWorkspaceId: string;
  triage: TriageSettings;
  work: WorkDefaults;
  quietHours: QuietHours | null;
  budget: WakeBudget;
  lookouts: Lookout[];
  charters: Charter[];
}

export const DEFAULT_TRIAGE_MODEL = "haiku";
export const MIN_INTERVAL_SECS = 30;
export const DEFAULT_INTERVAL_SECS = 300;

export function defaultBosunDraft(opts: {
  homeWorkspaceId?: string;
  workAgent?: Agent;
  workModel?: string;
  workEffort?: string;
} = {}): BosunDraft {
  const settings: ThreadSettings = {
    model: opts.workModel ?? "",
    ...(opts.workEffort ? { effort: opts.workEffort } : {}),
    access: "edits",
    mode: "build",
  };
  return {
    name: "Bosun",
    image: null,
    enabled: true,
    homeWorkspaceId: opts.homeWorkspaceId ?? "",
    triage: { agent: "claude", model: DEFAULT_TRIAGE_MODEL },
    work: { agent: opts.workAgent ?? "claude", settings },
    quietHours: null,
    budget: { maxTurnsPerHour: 6, maxConcurrent: 2 },
    lookouts: [],
    charters: [],
  };
}

export function draftFromBosun(b: Bosun): BosunDraft {
  return {
    id: b.id,
    name: b.name,
    image: b.image ?? null,
    enabled: b.enabled,
    homeWorkspaceId: b.homeWorkspaceId,
    triage: { ...b.triage },
    work: { agent: b.work.agent, settings: { ...b.work.settings } },
    quietHours: b.quietHours ? { ...b.quietHours } : null,
    budget: { ...b.budget },
    lookouts: b.lookouts.map((l) => ({ ...l, kind: { ...l.kind } as LookoutKind })),
    charters: b.charters.map((c) => ({ ...c, routeHints: [...c.routeHints] })),
  };
}

/** Trim the free-text bits the form edits line by line. */
export function normalizeDraft(d: BosunDraft): BosunDraft {
  return {
    ...d,
    name: d.name.trim(),
    lookouts: d.lookouts.map((l) =>
      l.kind.type === "command"
        ? { ...l, kind: { ...l.kind, args: l.kind.args.filter((a) => a.length > 0) } }
        : l,
    ),
    charters: d.charters.map((c) => ({
      ...c,
      routeHints: c.routeHints.map((h) => h.trim()).filter(Boolean),
    })),
  };
}

/** 32 random hex chars, for a webhook lookout's bearer secret. */
export function randomSecret(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function newId(): string {
  const c = globalThis.crypto as Crypto & { randomUUID?: () => string };
  return c.randomUUID ? c.randomUUID() : `lk-${randomSecret()}`;
}

export const LOOKOUT_TYPES: { id: LookoutType; label: string }[] = [
  { id: "command", label: "Command" },
  { id: "folder", label: "Folder" },
  { id: "webhook", label: "Webhook" },
  { id: "timer", label: "Timer" },
];

export function newLookout(type: LookoutType): Lookout {
  let kind: LookoutKind;
  switch (type) {
    case "command":
      kind = { type, command: "", args: [], env: {}, cwd: null };
      break;
    case "folder":
      kind = { type, path: "", pattern: "triage.md", maxAgeDays: 3 };
      break;
    case "webhook":
      kind = { type, secret: randomSecret() };
      break;
    case "timer":
      kind = { type, cadence: { type: "weekdays", time: "08:00" }, prompt: "", nextRunAt: null };
      break;
  }
  return {
    id: newId(),
    name: LOOKOUT_TYPES.find((t) => t.id === type)?.label ?? type,
    enabled: true,
    kind,
    intervalSecs: DEFAULT_INTERVAL_SECS,
    lastSignalCount: 0,
  };
}

export function newCharter(workspaceId: string): Charter {
  return {
    workspaceId,
    summary: "",
    routeHints: [],
    standingOrders: "",
    access: null,
    member: null,
    logThread: true,
    allowWork: true,
  };
}

const HHMM = /^([01]\d|2[0-3]):[0-5]\d$/;

export function isHHMM(s: string): boolean {
  return HHMM.test(s);
}

export function cadenceValid(c: Cadence): boolean {
  switch (c.type) {
    case "hourly":
      if (!Number.isInteger(c.everyHours) || c.everyHours < 1 || c.everyHours > 24) return false;
      break;
    case "weekly":
      if (!isHHMM(c.time) || c.days.length === 0) return false;
      if (c.days.some((d) => !Number.isInteger(d) || d < 0 || d > 6)) return false;
      break;
    default:
      if (!isHHMM(c.time)) return false;
  }
  return nextOccurrence(c, new Date(2026, 0, 5, 12, 0)) !== null;
}

export interface DraftIssue {
  /** Dotted path to the offending field, e.g. "lookouts.0.intervalSecs". */
  field: string;
  message: string;
}

/** Everything that would make save fail or produce a Bosun that can't run.
 *  Empty array = valid. `workspaces` (when given) checks the home is real. */
export function validateBosunDraft(
  d: BosunDraft,
  workspaces?: readonly Pick<Workspace, "id">[],
): DraftIssue[] {
  const issues: DraftIssue[] = [];
  const add = (field: string, message: string) => issues.push({ field, message });
  if (!d.name.trim()) add("name", "Give your Bosun a name.");
  if (!d.homeWorkspaceId) add("homeWorkspaceId", "Choose a home workspace.");
  else if (workspaces && !workspaces.some((w) => w.id === d.homeWorkspaceId))
    add("homeWorkspaceId", "The home workspace no longer exists.");
  if (!d.triage.model.trim()) add("triage.model", "Triage needs a model.");
  if (d.quietHours) {
    if (!isHHMM(d.quietHours.start)) add("quietHours.start", "Quiet hours start must be HH:MM.");
    if (!isHHMM(d.quietHours.end)) add("quietHours.end", "Quiet hours end must be HH:MM.");
  }
  if (!Number.isInteger(d.budget.maxTurnsPerHour) || d.budget.maxTurnsPerHour < 1)
    add("budget.maxTurnsPerHour", "Turns per hour must be at least 1.");
  if (!Number.isInteger(d.budget.maxConcurrent) || d.budget.maxConcurrent < 1)
    add("budget.maxConcurrent", "Concurrent threads must be at least 1.");
  d.lookouts.forEach((l, i) => {
    const at = `lookouts.${i}`;
    const label = l.name.trim() || `Lookout ${i + 1}`;
    switch (l.kind.type) {
      case "command":
        if (!l.kind.command.trim()) add(`${at}.command`, `${label}: command is empty.`);
        break;
      case "folder":
        if (!l.kind.path.trim()) add(`${at}.path`, `${label}: folder path is empty.`);
        if (!l.kind.pattern.trim()) add(`${at}.pattern`, `${label}: file pattern is empty.`);
        if (!Number.isInteger(l.kind.maxAgeDays) || l.kind.maxAgeDays < 1)
          add(`${at}.maxAgeDays`, `${label}: max age must be at least 1 day.`);
        break;
      case "webhook":
        if (!l.kind.secret.trim()) add(`${at}.secret`, `${label}: webhook secret is empty.`);
        break;
      case "timer":
        if (!cadenceValid(l.kind.cadence)) add(`${at}.cadence`, `${label}: timer cadence is invalid.`);
        break;
    }
    if (
      (l.kind.type === "command" || l.kind.type === "folder") &&
      (!Number.isFinite(l.intervalSecs) || l.intervalSecs < MIN_INTERVAL_SECS)
    )
      add(`${at}.intervalSecs`, `${label}: interval must be at least ${MIN_INTERVAL_SECS} s.`);
  });
  const seen = new Set<string>();
  d.charters.forEach((c, i) => {
    if (seen.has(c.workspaceId))
      add(`charters.${i}.workspaceId`, "Only one charter per workspace.");
    seen.add(c.workspaceId);
  });
  return issues;
}

// ---- small display helpers -----------------------------------------------

export const CHARTER_ACCESS: { id: Access | ""; label: string }[] = [
  { id: "", label: "Inherit" },
  { id: "read", label: "Read-only" },
  { id: "edits", label: "Edits allowed" },
  { id: "full", label: "Full access" },
];

export const DECISION_LABEL: Record<DecisionKind, string> = {
  ignore: "ignored",
  log: "logged",
  work: "work",
  ask: "ask",
};

/** "KEY=value" lines ↔ env map, for the command lookout's env textarea. */
export function envToText(env: Record<string, string>): string {
  return Object.entries(env)
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
}

export function textToEnv(text: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const eq = line.indexOf("=");
    if (eq <= 0) continue;
    const key = line.slice(0, eq).trim();
    if (key) env[key] = line.slice(eq + 1);
  }
  return env;
}


// ---- tasks (docs/BOSUN.md §10.7) -----------------------------------------

/** Display order of the task list's groups. Snoozed and Done start collapsed. */
export type TaskGroupId = "needs_you" | "working" | "todo" | "waiting_on_client" | "snoozed" | "done";

export const TASK_GROUPS: { id: TaskGroupId; label: string; collapsed: boolean }[] = [
  { id: "needs_you", label: "Needs you", collapsed: false },
  { id: "working", label: "Working", collapsed: false },
  { id: "todo", label: "To do", collapsed: false },
  { id: "waiting_on_client", label: "Waiting on client", collapsed: false },
  { id: "snoozed", label: "Snoozed", collapsed: true },
  { id: "done", label: "Done (3 days)", collapsed: true },
];

/** `activeTaskId` value that puts the Review to-dos page in the main pane
 *  instead of one task, so every path that closes a task page closes it too. */
export const TASK_REVIEW_ID = "__review__";

/** `activeTaskId` value for the Today page (the day plan), same idea. */
export const TASK_TODAY_ID = "__today__";

/** The page a task was opened from, so the task page can offer the way back.
 *  sessionStorage, so a phone WebView reloading in the background keeps it. */
export type TaskReturnPage = "review" | "today";
const TASK_RETURN_KEY = "threadknot.bosunTask.return";

export function taskReturnPage(): TaskReturnPage | null {
  try {
    const v = sessionStorage.getItem(TASK_RETURN_KEY);
    return v === "review" || v === "today" ? v : null;
  } catch {
    return null;
  }
}

export function setTaskReturnPage(page: TaskReturnPage | null) {
  try {
    if (page) sessionStorage.setItem(TASK_RETURN_KEY, page);
    else sessionStorage.removeItem(TASK_RETURN_KEY);
  } catch {
    // Storage disabled: the task page just offers no way back.
  }
}

export const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  needs_you: "Needs you",
  working: "Working",
  todo: "To do",
  waiting_on_client: "Waiting on client",
  snoozed: "Snoozed",
  done: "Done",
  dropped: "Dropped",
};

/** The statuses a person picks from in the task view (Snoozed goes through
 *  the snooze menu, since it needs a date). */
export const PICKABLE_STATUSES: TaskStatus[] = ["needs_you", "working", "todo", "waiting_on_client", "done"];

export function isClosedStatus(status: TaskStatus): boolean {
  return status === "done" || status === "dropped";
}

/** Open = something is or will be done about it: not closed, not snoozed. */
export function isOpenTask(task: Pick<BosunTask, "status">): boolean {
  return !isClosedStatus(task.status) && task.status !== "snoozed";
}

/** A to-do the person typed in (only those may be deleted outright). */
export function isManualTask(task: Pick<BosunTask, "key" | "source">): boolean {
  return task.source?.kind === "manual" || task.key.startsWith("adhoc:");
}

export function taskGroupOf(status: TaskStatus): TaskGroupId {
  return status === "dropped" ? "done" : status;
}

const CLOSED_WINDOW_MS = 3 * 24 * 60 * 60 * 1000;

/** Sort key for an ISO date or timestamp; a bare "YYYY-MM-DD" is read as
 *  local midnight so "due today" compares as today wherever this runs. */
function dueMs(due: string | null | undefined): number | null {
  if (!due) return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(due);
  if (m) return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime();
  const t = Date.parse(due);
  return Number.isFinite(t) ? t : null;
}

/** Within a group: due soonest first (undated after dated), then the most
 *  recently updated. */
export function compareTasks(a: BosunTask, b: BosunTask): number {
  const da = dueMs(a.due);
  const db = dueMs(b.due);
  if (da !== null && db !== null && da !== db) return da - db;
  if (da !== null && db === null) return -1;
  if (da === null && db !== null) return 1;
  return isoMs(b.updatedAt) - isoMs(a.updatedAt);
}

export interface TaskGroup {
  id: TaskGroupId;
  label: string;
  /** Default fold state; the view may override per group. */
  collapsed: boolean;
  tasks: BosunTask[];
}

/** Tasks bucketed into the list's groups, in display order, each sorted by
 *  `compareTasks`. Empty groups are dropped. Closed tasks older than three
 *  days (by `doneAt`, else `updatedAt`) are left out even if the server sent
 *  them. */
export function groupTasks(tasks: readonly BosunTask[], now: Date = new Date()): TaskGroup[] {
  const buckets = new Map<TaskGroupId, BosunTask[]>();
  const cutoff = now.getTime() - CLOSED_WINDOW_MS;
  for (const t of tasks) {
    if (isClosedStatus(t.status) && isoMs(t.doneAt ?? t.updatedAt) < cutoff) continue;
    const g = taskGroupOf(t.status);
    if (!TASK_GROUPS.some((x) => x.id === g)) continue;
    let list = buckets.get(g);
    if (!list) buckets.set(g, (list = []));
    list.push(t);
  }
  return TASK_GROUPS.filter((g) => (buckets.get(g.id)?.length ?? 0) > 0).map((g) => ({
    ...g,
    tasks: buckets.get(g.id)!.sort(compareTasks),
  }));
}

/** "just now" / "5m ago" / "3h ago" / "2d ago". */
export function ageLabel(iso: string, now: Date = new Date()): string {
  const ms = now.getTime() - isoMs(iso);
  const min = Math.floor(ms / 60_000);
  if (min < 1) return "just now";
  if (min < 60) return `${min}m ago`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

/** "due today" / "due tomorrow" / "due Oct 5" / "overdue Sep 28". */
export function dueLabel(due: string, now: Date = new Date()): string {
  const ms = dueMs(due);
  if (ms === null) return `due ${due}`;
  const d = new Date(ms);
  const day = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const diff = Math.round((day - today) / 86_400_000);
  const date = `${MONTHS[d.getMonth()]} ${d.getDate()}${d.getFullYear() === now.getFullYear() ? "" : ` ${d.getFullYear()}`}`;
  if (diff < 0) return `overdue ${date}`;
  if (diff === 0) return "due today";
  if (diff === 1) return "due tomorrow";
  return `due ${date}`;
}

/** The row's muted second line: `client · workspace · due or age`, leaving
 *  out whatever is unknown. Closed tasks say when they closed instead. */
export function taskMetaLine(
  task: BosunTask,
  workspaceName: string | null | undefined,
  now: Date = new Date(),
): string {
  const parts: string[] = [];
  if (task.client?.trim()) parts.push(task.client.trim());
  if (workspaceName?.trim()) parts.push(workspaceName.trim());
  if (task.status === "snoozed" && task.snoozedUntil) parts.push(`until ${snoozeLabel(task.snoozedUntil, now)}`);
  else if (isClosedStatus(task.status)) parts.push(`${TASK_STATUS_LABEL[task.status].toLowerCase()} ${ageLabel(task.doneAt ?? task.updatedAt, now)}`);
  else if (task.due) parts.push(dueLabel(task.due, now));
  else parts.push(ageLabel(task.createdAt, now));
  return parts.join(" · ");
}

/** "Thu 8:00" within a week, else "Oct 12 8:00". */
export function snoozeLabel(iso: string, now: Date = new Date()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const time = `${d.getHours()}:${pad2(d.getMinutes())}`;
  const days = (d.getTime() - now.getTime()) / 86_400_000;
  if (days >= 0 && days < 6) return `${WEEKDAYS[d.getDay()]} ${time}`;
  return `${MONTHS[d.getMonth()]} ${d.getDate()} ${time}`;
}

/** Local 8:00 on the given local calendar day. */
function at8(y: number, m: number, d: number): Date {
  return new Date(y, m, d, 8, 0, 0, 0);
}

/** Snooze presets in local time: tomorrow 8am, and next Monday 8am (a week
 *  out when today is Monday). ISO strings, ready for `snoozedUntil`. */
export function snoozePresets(now: Date = new Date()): { id: "tomorrow" | "monday"; label: string; until: string }[] {
  const tomorrow = at8(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  const ahead = ((8 - now.getDay()) % 7) || 7; // days until the next Monday
  const monday = at8(now.getFullYear(), now.getMonth(), now.getDate() + ahead);
  return [
    { id: "tomorrow", label: "Tomorrow 8am", until: tomorrow.toISOString() },
    { id: "monday", label: "Next Monday 8am", until: monday.toISOString() },
  ];
}

/** A date input's "YYYY-MM-DD" as local 8:00, ISO. Null when malformed. */
export function snoozeUntilDate(date: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return null;
  return at8(Number(m[1]), Number(m[2]) - 1, Number(m[3])).toISOString();
}

export interface WorkspaceRollup {
  needYou: number;
  open: number;
}

/** Per workspace id: how many open tasks need you, and how many are open at
 *  all. Tasks with no workspace are not counted anywhere. */
export function workspaceTaskRollup(tasks: readonly BosunTask[]): Map<string, WorkspaceRollup> {
  const out = new Map<string, WorkspaceRollup>();
  for (const t of tasks) {
    if (!t.workspaceId || !isOpenTask(t)) continue;
    let r = out.get(t.workspaceId);
    if (!r) out.set(t.workspaceId, (r = { needYou: 0, open: 0 }));
    r.open += 1;
    if (t.status === "needs_you") r.needYou += 1;
  }
  return out;
}

/** The collapsed workspace row's label: `Blackbeard · 2 need you`, or
 *  `Blackbeard · 3 open` (muted) when nothing needs you. Null = no row. */
export function bosunRowLabel(
  bosunName: string,
  rollup: WorkspaceRollup | undefined,
): { text: string; muted: boolean } | null {
  if (!rollup || rollup.open === 0) return null;
  if (rollup.needYou > 0) return { text: `${bosunName} · ${rollup.needYou} need${rollup.needYou === 1 ? "s" : ""} you`, muted: false };
  return { text: `${bosunName} · ${rollup.open} open`, muted: true };
}

/** Split a workspace's threads into what the list shows and the Bosun
 *  threads (day logs included) it folds into the one collapsed row. A Bosun
 *  thread `keepInline` accepts (one you were just working in, see
 *  `recentBosunThreadFilter`) stays in `shown`, in its normal place. */
export function splitBosunThreads<T extends Pick<Thread, "origin">>(
  threads: readonly T[],
  keepInline?: (thread: T) => boolean,
): { shown: T[]; bosun: T[] } {
  const shown: T[] = [];
  const bosun: T[] = [];
  for (const t of threads) {
    (t.origin?.kind === "bosun" && !keepInline?.(t) ? bosun : shown).push(t);
  }
  return { shown, bosun };
}

/** Sidebar search over tasks: title, client, summary (already lowercased
 *  query, like the thread filter). */
export function taskMatches(task: BosunTask, query: string): boolean {
  if (!query) return true;
  return [task.title, task.client ?? "", task.summary].some((s) => s.toLowerCase().includes(query));
}

/** Tasks answering "need you" for the launch button and rail dot. */
export function needsYouCount(tasks: readonly BosunTask[]): number {
  return tasks.reduce((n, t) => n + (t.status === "needs_you" ? 1 : 0), 0);
}

// ---- task page: what the thread says ------------------------------------

/** The last finished assistant message in a thread tail (blank ones skipped),
 *  for the task page's "Latest from Bosun". Null when there is none. */
export function lastAssistantMessage(
  events: readonly PersistedEvent[],
): { text: string; ts: string; seq: number } | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.event.kind === "assistant_message" && e.event.text.trim()) {
      return { text: e.event.text, ts: e.ts, seq: e.seq };
    }
  }
  return null;
}

export interface PendingQuestion {
  requestId: string;
  questions: Question[];
  ts: string;
  seq: number;
}

/** The question the thread is waiting on, if any. A `question_request` is
 *  answerable only while its turn is live: the feed marks it answered on a
 *  matching `question_resolved`, and the server drops it (thread idle) on
 *  `turn_completed` / `turn_aborted` / `error`; a later `turn_started` means
 *  a new turn has begun past it. Any of those closes it. */
export function pendingQuestion(events: readonly PersistedEvent[]): PendingQuestion | null {
  let open: PendingQuestion | null = null;
  const resolved = new Set<string>();
  for (const e of events) {
    const ev = e.event;
    switch (ev.kind) {
      case "question_request":
        // Replays may double-fire a request that was already answered.
        if (!resolved.has(ev.requestId) && ev.questions.length > 0) {
          open = { requestId: ev.requestId, questions: ev.questions, ts: e.ts, seq: e.seq };
        }
        break;
      case "question_resolved":
        resolved.add(ev.requestId);
        if (open?.requestId === ev.requestId) open = null;
        break;
      case "turn_started":
      case "turn_completed":
      case "turn_aborted":
      case "error":
        open = null;
        break;
      default:
        break;
    }
  }
  return open;
}

const MIGRATION_NOTE = /^created from an existing bosun thread\.?$/i;

/** History worth reading: the migration's "Created from an existing Bosun
 *  thread" entry is noise once anything else has happened, so it is dropped
 *  whenever other entries exist. Order is kept (oldest first). */
export function visibleHistory<T extends Pick<TaskEvent, "note">>(history: readonly T[]): T[] {
  const rest = history.filter((ev) => !MIGRATION_NOTE.test(ev.note.trim()));
  return rest.length > 0 ? rest : history.slice();
}

// ---- recent touches (per device) -----------------------------------------
//
// What this device last worked on, so a task you started is easy to find
// again after walking away: the task list's Recent group, the rail tile's
// "open the last one", and the workspace lists' inline Bosun threads all read
// these. Stored in localStorage, newest first, one entry per id.

export const LS_BOSUN_RECENT = "threadknot.bosunRecent";
/** Bosun threads opened here (day logs and pre-task threads have no task). */
export const LS_BOSUN_RECENT_THREADS = "threadknot.bosunRecentThreads";
export const RECENT_MAX = 30;
/** How far back the Recent group looks. */
export const RECENT_WINDOW_MS = 3 * 24 * 60 * 60 * 1000;
/** How long a touched Bosun thread stays inline in its workspace list. */
export const RECENT_INLINE_MS = 24 * 60 * 60 * 1000;
export const RECENT_LIMIT = 6;

export interface RecentTouch {
  taskId: string;
  at: string;
}

export interface RecentThreadTouch {
  threadId: string;
  at: string;
}

/** Newest first, one entry per key (its newest `at` wins), at most `max`. */
function normalizeRecent<T extends { at: string }>(
  list: readonly T[],
  key: (e: T) => string,
  max: number,
): T[] {
  const best = new Map<string, T>();
  for (const e of list) {
    const k = key(e);
    const prev = best.get(k);
    if (!prev || isoMs(e.at) > isoMs(prev.at)) best.set(k, e);
  }
  return [...best.values()].sort((a, b) => isoMs(b.at) - isoMs(a.at)).slice(0, max);
}

/** Record a touch of `taskId` at `at`: deduped, newest first, capped. */
export function addRecentTouch(
  list: readonly RecentTouch[],
  taskId: string,
  at: string,
  max = RECENT_MAX,
): RecentTouch[] {
  return normalizeRecent([{ taskId, at }, ...list], (e) => e.taskId, max);
}

export function addRecentThreadTouch(
  list: readonly RecentThreadTouch[],
  threadId: string,
  at: string,
  max = RECENT_MAX,
): RecentThreadTouch[] {
  return normalizeRecent([{ threadId, at }, ...list], (e) => e.threadId, max);
}

/** Parse a stored list, dropping anything malformed. Never throws. */
export function parseRecentTouches(raw: string | null): RecentTouch[] {
  return parseStored(raw, "taskId") as unknown as RecentTouch[];
}

export function parseRecentThreadTouches(raw: string | null): RecentThreadTouch[] {
  return parseStored(raw, "threadId") as unknown as RecentThreadTouch[];
}

function parseStored(raw: string | null, field: "taskId" | "threadId"): Record<string, string>[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const ok = parsed.filter(
      (e): e is Record<string, string> =>
        !!e &&
        typeof e === "object" &&
        typeof (e as Record<string, unknown>)[field] === "string" &&
        !!(e as Record<string, string>)[field] &&
        typeof (e as Record<string, unknown>).at === "string" &&
        Number.isFinite(Date.parse((e as Record<string, string>).at)),
    );
    return normalizeRecent(
      ok.map((e) => ({ [field]: e[field], at: e.at })),
      (e) => e[field],
      RECENT_MAX,
    );
  } catch {
    return [];
  }
}

// The storage side. Listeners fire on this window's own writes and on other
// windows' writes (the `storage` event), so every list stays in step.
const recentListeners = new Set<() => void>();
let recentVersion = 0;
let storageHooked = false;

function bumpRecent() {
  recentVersion += 1;
  for (const l of [...recentListeners]) l();
}

/** For `useSyncExternalStore`: changes whenever either list is written. */
export function bosunRecentVersion(): number {
  return recentVersion;
}

export function subscribeBosunRecent(listener: () => void): () => void {
  recentListeners.add(listener);
  if (!storageHooked && typeof window !== "undefined") {
    storageHooked = true;
    window.addEventListener("storage", (e) => {
      if (e.key === LS_BOSUN_RECENT || e.key === LS_BOSUN_RECENT_THREADS) bumpRecent();
    });
  }
  return () => {
    recentListeners.delete(listener);
  };
}

function readLS(key: string): string | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeLS(key: string, value: unknown) {
  try {
    if (typeof localStorage !== "undefined") localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // A convenience only.
  }
}

export function loadRecentTouches(): RecentTouch[] {
  return parseRecentTouches(readLS(LS_BOSUN_RECENT));
}

export function loadRecentThreadTouches(): RecentThreadTouch[] {
  return parseRecentThreadTouches(readLS(LS_BOSUN_RECENT_THREADS));
}

/** This device touched a task: opened it, worked it, answered it, moved it. */
export function touchTask(taskId: string, now: Date = new Date()): void {
  if (!taskId) return;
  writeLS(LS_BOSUN_RECENT, addRecentTouch(loadRecentTouches(), taskId, now.toISOString()));
  bumpRecent();
}

/** This device opened a Bosun thread (its task, if any, is touched too). */
export function touchBosunThread(
  thread: Pick<Thread, "id" | "origin">,
  now: Date = new Date(),
): void {
  if (thread.origin?.kind !== "bosun") return;
  writeLS(
    LS_BOSUN_RECENT_THREADS,
    addRecentThreadTouch(loadRecentThreadTouches(), thread.id, now.toISOString()),
  );
  if (thread.origin.taskId) touchTask(thread.origin.taskId, now);
  else bumpRecent();
}

export interface RecentTask {
  task: BosunTask;
  /** Your last touch on this device. */
  touchedAt: string;
  /** The Bosun updated the task after that touch (newest history by "bosun"). */
  replied: boolean;
  /** Sort key: the task's update when replied, else your touch. */
  at: string;
}

/** The Recent group: tasks touched in the last three days, newest activity
 *  first, where activity is your touch or, when Blackbeard has done something
 *  since, the task's `updatedAt`. */
export function recentTasks(
  tasks: readonly BosunTask[],
  touches: readonly RecentTouch[],
  now: Date = new Date(),
  limit = RECENT_LIMIT,
  includeFinished = false,
): RecentTask[] {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const cutoff = now.getTime() - RECENT_WINDOW_MS;
  const out: RecentTask[] = [];
  for (const touch of normalizeRecent(touches, (e) => e.taskId, RECENT_MAX)) {
    const task = byId.get(touch.taskId);
    if (!task || isoMs(touch.at) < cutoff) continue;
    // Marking something done is how you clear it from Recent.
    if (!includeFinished && (task.status === "done" || task.status === "dropped")) continue;
    const last = task.history[task.history.length - 1];
    const replied = isoMs(task.updatedAt) > isoMs(touch.at) && last?.by === "bosun";
    out.push({ task, touchedAt: touch.at, replied, at: replied ? task.updatedAt : touch.at });
  }
  return out.sort((a, b) => isoMs(b.at) - isoMs(a.at)).slice(0, limit);
}

/** Which folded Bosun threads a workspace list shows inline anyway: ones
 *  opened here in the last day, or whose task was touched in the last day. */
export function recentBosunThreadFilter(
  touches: readonly RecentTouch[],
  threadTouches: readonly RecentThreadTouch[],
  now: Date = new Date(),
  windowMs = RECENT_INLINE_MS,
): (thread: Pick<Thread, "id" | "origin">) => boolean {
  const cutoff = now.getTime() - windowMs;
  const tasks = new Set(touches.filter((e) => isoMs(e.at) >= cutoff).map((e) => e.taskId));
  const threads = new Set(threadTouches.filter((e) => isoMs(e.at) >= cutoff).map((e) => e.threadId));
  return (t) => threads.has(t.id) || (!!t.origin?.taskId && tasks.has(t.origin.taskId));
}

/** Per-device "Show finished" toggle for the Recent group. */
const RECENT_FINISHED_KEY = "threadknot.bosunRecentShowFinished";
export function getRecentShowFinished(): boolean {
  try {
    return localStorage.getItem(RECENT_FINISHED_KEY) === "1";
  } catch {
    return false;
  }
}
export function setRecentShowFinished(on: boolean): void {
  try {
    localStorage.setItem(RECENT_FINISHED_KEY, on ? "1" : "0");
  } catch {
    /* storage unavailable: the toggle just won't persist */
  }
}

const TODAY_THREAD_KEY = "threadknot.bosunToday.thread";

/** The conversation last opened from Today, so that thread can offer
 *  "← Today". Keyed to the one thread, so a stale flag never shows it on
 *  another. */
export function todayReturnThread(): string | null {
  try {
    return sessionStorage.getItem(TODAY_THREAD_KEY);
  } catch {
    return null;
  }
}

function setTodayReturnThread(threadId: string | null) {
  try {
    if (threadId) sessionStorage.setItem(TODAY_THREAD_KEY, threadId);
    else sessionStorage.removeItem(TODAY_THREAD_KEY);
  } catch {
    // ignore
  }
}

/** Clicking a to-do in a list: straight into its conversation when it has
 *  one (that is where the work is), else its task page. `from` is the page
 *  the click came from, for the way back. */
export function openTaskOrConversation(
  task: Pick<BosunTask, "id" | "threadId">,
  actions: { openTask: (taskId: string | null) => void; selectThread: (threadId: string) => Promise<boolean> },
  from: TaskReturnPage | null,
): void {
  setTaskReturnPage(from);
  if (task.threadId) {
    touchTask(task.id);
    setTodayReturnThread(from === "today" ? task.threadId : null);
    // A thread that is gone (pruned, deleted) falls back to the task page.
    void actions
      .selectThread(task.threadId)
      .then((ok) => {
        if (!ok) actions.openTask(task.id);
      })
      .catch(() => actions.openTask(task.id));
  } else {
    setTodayReturnThread(null);
    actions.openTask(task.id);
  }
}

/** After closing a to-do: a client ticket Orbit kept open (docs/BOSUN.md
 *  §10.8) gets a plain confirm, then the forced close. For pages without
 *  their own inline prompt (the task page, a thread's to-do bar). */
export async function confirmHeldOrbitClose(
  res: { id: string; orbitClose?: import("./protocol").OrbitCloseOutcome },
  closeInOrbit: (taskId: string) => Promise<import("./protocol").OrbitCloseOutcome>,
): Promise<void> {
  const oc = res.orbitClose;
  if (!oc) return;
  const ticket = oc.ticket ? `#${oc.ticket}` : "ticket";
  if (oc.error) {
    window.alert(`Done here, but Orbit ${ticket} is still open: ${oc.error}`);
    return;
  }
  if (!oc.needsConfirm) return;
  const yes = window.confirm(
    `Orbit ${ticket} is a ${oc.origin ?? "client"} ticket, and closing it may email them. Close it in Orbit too?`,
  );
  if (!yes) return;
  const out = await closeInOrbit(res.id);
  if (!out.closed) window.alert(`Orbit kept ${ticket} open${out.error ? `: ${out.error}` : "."}`);
}
