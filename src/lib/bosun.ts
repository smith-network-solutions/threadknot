// Pure helpers for the Bosun UI (docs/BOSUN.md §8): the sidebar timeline, the
// wake summary line, the create-form draft and its validation. Nothing here
// touches the DOM or the socket, so scripts/test-bosun-ui.mjs can import it
// straight through esbuild.

import type {
  Access,
  Agent,
  Bosun,
  Cadence,
  Charter,
  DecisionKind,
  Lookout,
  LookoutKind,
  LookoutType,
  QuietHours,
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

export type TimelineItem =
  | { kind: "wake"; at: string; wake: Wake }
  | { kind: "thread"; at: string; thread: Thread };

export interface TimelineDay {
  key: string;
  items: TimelineItem[];
}

/** Wakes and the threads bosuns opened, grouped by local day (newest day
 *  first), each day's entries interleaved newest first. A thread is placed
 *  by `createdAt`: the day its wake opened it. */
export function bosunTimeline(wakes: readonly Wake[], threads: readonly Thread[]): TimelineDay[] {
  const byDay = new Map<string, TimelineItem[]>();
  const push = (item: TimelineItem) => {
    const key = localDayKey(item.at);
    const list = byDay.get(key);
    if (list) list.push(item);
    else byDay.set(key, [item]);
  };
  for (const wake of wakes) push({ kind: "wake", at: wake.at, wake });
  for (const thread of threads) {
    if (isBosunThread(thread)) push({ kind: "thread", at: thread.createdAt, thread });
  }
  const ms = (iso: string) => {
    const t = Date.parse(iso);
    return Number.isFinite(t) ? t : 0;
  };
  return [...byDay.entries()]
    .sort(([a], [b]) => (a < b ? 1 : a > b ? -1 : 0))
    .map(([key, items]) => ({
      key,
      items: items.sort((a, b) => ms(b.at) - ms(a.at)),
    }));
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
