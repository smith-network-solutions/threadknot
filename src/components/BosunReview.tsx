import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { BosunTask, TaskStatus } from "../lib/protocol";
import {
  TASK_STATUS_LABEL,
  ageLabel,
  displayTitle,
  isClosedStatus,
  setTaskReturnPage,
  taskMatches,
  taskReturnPage,
} from "../lib/bosun";
import { allWorkspaces, useStore } from "../state/store";
import { orbitTicketLabel, TaskStatusDot } from "./BosunTaskView";
import { CheckIcon, ChevronIcon, PanelLeftIcon, XIcon } from "./icons";
import "../styles/bosun-review.css";

/** The statuses the review filters by, in the sidebar's group order. Closed
 *  tasks are what this view produces, not what it reviews. */
const REVIEW_STATUSES: TaskStatus[] = ["needs_you", "todo", "waiting_on_client", "working", "snoozed"];

const AGE_CHOICES: { days: number; label: string }[] = [
  { days: 0, label: "Any age" },
  { days: 3, label: "Untouched 3+ days" },
  { days: 7, label: "Untouched 7+ days" },
  { days: 14, label: "Untouched 14+ days" },
];

const DAY_MS = 24 * 60 * 60 * 1000;

/** What the review was showing, kept across opening a task and coming back
 *  (the page unmounts while a task is open). sessionStorage so a phone's
 *  WebView reloading in the background doesn't lose it either. */
interface ReviewSession {
  query: string;
  statuses: TaskStatus[];
  workspaceId: string;
  ageDays: number;
  oldestFirst: boolean;
  selected: string[];
  scrollTop: number;
}
const SESSION_KEY = "threadknot.bosunReview";

function loadSession(): ReviewSession | null {
  try {
    const raw = sessionStorage.getItem(SESSION_KEY);
    return raw ? (JSON.parse(raw) as ReviewSession) : null;
  } catch {
    return null;
  }
}

function saveSession(s: ReviewSession) {
  try {
    sessionStorage.setItem(SESSION_KEY, JSON.stringify(s));
  } catch {
    // Storage full or disabled: the review still works, it just forgets.
  }
}

/** Whether the open task was reached from the review, so the task page can
 *  offer the way back. Set when a row is opened, cleared when the review is
 *  closed. */
export function cameFromReview(): boolean {
  return taskReturnPage() === "review";
}

function setCameFromReview(on: boolean) {
  setTaskReturnPage(on ? "review" : null);
}
type HeldTicket = { taskId: string; ticket: string };

/** Requests in flight at once during a bulk close: quick, without queueing
 *  two hundred writes against the task file at the same instant. */
const BULK_CONCURRENCY = 6;

/** The main pane for going through a pile of to-dos at once: every open task
 *  with its whole title, filters to narrow the pile, checkboxes, and one bar
 *  to close the selection. Laid out for a phone first; a row is a full-width
 *  tap target and the bulk bar sits at the bottom within thumb reach. */
export function BosunReview() {
  const { state, dispatch, actions } = useStore();
  const [initial] = useState(loadSession);
  const [query, setQuery] = useState(initial?.query ?? "");
  const [statuses, setStatuses] = useState<Set<TaskStatus>>(
    () => new Set<TaskStatus>(initial?.statuses ?? ["needs_you", "todo", "waiting_on_client"]),
  );
  const [workspaceId, setWorkspaceId] = useState(initial?.workspaceId ?? "");
  const [ageDays, setAgeDays] = useState(initial?.ageDays ?? 0);
  const [oldestFirst, setOldestFirst] = useState(initial?.oldestFirst ?? true);
  const [selected, setSelected] = useState<Set<string>>(() => new Set(initial?.selected ?? []));
  const listRef = useRef<HTMLDivElement>(null);
  const scrollTop = useRef(initial?.scrollTop ?? 0);
  const [progress, setProgress] = useState<{ done: number; total: number; verb: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Client tickets a bulk close left open in Orbit (§10.8), to confirm. */
  const [held, setHeld] = useState<HeldTicket[]>([]);
  const [closingHeld, setClosingHeld] = useState(false);

  async function closeHeld() {
    setClosingHeld(true);
    const failed: string[] = [];
    for (const h of held) {
      try {
        const oc = await actions.closeTaskInOrbit(h.taskId);
        if (!oc.closed) failed.push(h.ticket);
      } catch {
        failed.push(h.ticket);
      }
    }
    setClosingHeld(false);
    setHeld([]);
    if (failed.length) setError(`Orbit kept ${failed.length} open: ${failed.join(", ")}`);
  }

  useEffect(() => {
    saveSession({
      query,
      statuses: [...statuses],
      workspaceId,
      ageDays,
      oldestFirst,
      selected: [...selected],
      scrollTop: scrollTop.current,
    });
  }, [query, statuses, workspaceId, ageDays, oldestFirst, selected]);

  // Back where you were in the list once the rows exist (tasks may still be
  // loading on the first render).
  const restored = useRef(false);
  const loaded = state.bosunTasks !== null;
  useLayoutEffect(() => {
    if (restored.current || !loaded || !listRef.current) return;
    restored.current = true;
    listRef.current.scrollTop = scrollTop.current;
  }, [loaded]);

  function openRow(id: string) {
    scrollTop.current = listRef.current?.scrollTop ?? scrollTop.current;
    saveSession({
      query,
      statuses: [...statuses],
      workspaceId,
      ageDays,
      oldestFirst,
      selected: [...selected],
      scrollTop: scrollTop.current,
    });
    setCameFromReview(true);
    actions.openTask(id);
  }

  const workspaces = allWorkspaces(state);
  const wsName = (id: string | null | undefined) =>
    id ? (workspaces.find((w) => w.id === id)?.name ?? null) : null;

  const open = useMemo(
    () => (state.bosunTasks ?? []).filter((t) => !isClosedStatus(t.status)),
    [state.bosunTasks],
  );
  const statusCounts = useMemo(() => {
    const counts = new Map<TaskStatus, number>();
    for (const t of open) counts.set(t.status, (counts.get(t.status) ?? 0) + 1);
    return counts;
  }, [open]);
  const workspaceIds = useMemo(
    () => [...new Set(open.map((t) => t.workspaceId).filter((x): x is string => !!x))],
    [open],
  );

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    const cutoff = Date.now() - ageDays * DAY_MS;
    const list = open.filter(
      (t) =>
        statuses.has(t.status) &&
        (!workspaceId || t.workspaceId === workspaceId) &&
        (!ageDays || Date.parse(t.updatedAt) < cutoff) &&
        taskMatches(t, q),
    );
    list.sort((a, b) =>
      oldestFirst ? (a.updatedAt < b.updatedAt ? -1 : 1) : a.updatedAt < b.updatedAt ? 1 : -1,
    );
    return list;
  }, [open, statuses, workspaceId, ageDays, query, oldestFirst]);

  // Only what is still on screen counts as selected, so narrowing the filter
  // can never close something you can no longer see.
  const selectedShown = shown.filter((t) => selected.has(t.id));
  const allSelected = shown.length > 0 && selectedShown.length === shown.length;
  const busy = progress !== null;

  function toggle(id: string) {
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    setSelected(allSelected ? new Set() : new Set(shown.map((t) => t.id)));
  }

  function toggleStatus(s: TaskStatus) {
    setStatuses((cur) => {
      const next = new Set(cur);
      if (next.has(s)) next.delete(s);
      else next.add(s);
      return next;
    });
  }

  async function close(tasks: BosunTask[], status: "done" | "dropped") {
    if (tasks.length === 0) return;
    if (
      status === "dropped" &&
      !window.confirm(`Drop ${tasks.length} to-do${tasks.length === 1 ? "" : "s"}? They stop being tracked.`)
    )
      return;
    const verb = status === "done" ? "Marking done" : "Dropping";
    setError(null);
    setProgress({ done: 0, total: tasks.length, verb });
    const failed: string[] = [];
    const heldBack: HeldTicket[] = [];
    let next = 0;
    let finished = 0;
    setHeld([]);
    const worker = async () => {
      while (next < tasks.length) {
        const t = tasks[next++];
        try {
          const res = await actions.updateTask({ taskId: t.id, status });
          if (res.orbitClose?.needsConfirm) {
            heldBack.push({ taskId: t.id, ticket: res.orbitClose.ticket ? `#${res.orbitClose.ticket}` : (orbitTicketLabel(t) ?? "ticket") });
          }
        } catch {
          failed.push(displayTitle(t) || t.id);
        }
        finished += 1;
        setProgress({ done: finished, total: tasks.length, verb });
      }
    };
    await Promise.all(Array.from({ length: Math.min(BULK_CONCURRENCY, tasks.length) }, worker));
    setProgress(null);
    setHeld(heldBack);
    setSelected((s) => {
      const left = new Set(s);
      for (const t of tasks) left.delete(t.id);
      return left;
    });
    if (failed.length) {
      setError(`${failed.length} could not be updated: ${failed.slice(0, 3).join("; ")}${failed.length > 3 ? "…" : ""}`);
    }
  }

  return (
    <section className="thread-pane task-pane review-pane">
      <header className="thread-head task-head">
        <div className="head-cluster">
          <button
            className="icon-btn hamburger"
            aria-label="Open sidebar"
            onClick={() => dispatch({ type: "sidebar", open: true })}
          >
            <PanelLeftIcon size={18} />
          </button>
        </div>
        <div className="thread-head-main task-head-main">
          <span className="task-head-who">
            Review to-dos <span className="review-head-count">{open.length} open</span>
          </span>
        </div>
        <button
          type="button"
          className="icon-btn"
          aria-label="Close review"
          title="Close"
          onClick={() => {
            setCameFromReview(false);
            actions.openTask(null);
          }}
        >
          <XIcon size={15} />
        </button>
      </header>

      <div className="review-tools">
        <input
          type="search"
          className="review-search"
          placeholder="Search title, client, summary"
          aria-label="Search to-dos"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="review-chips" role="group" aria-label="Status">
          {REVIEW_STATUSES.filter((s) => (statusCounts.get(s) ?? 0) > 0).map((s) => (
            <button
              key={s}
              type="button"
              className={`review-chip s-${s}${statuses.has(s) ? " on" : ""}`}
              aria-pressed={statuses.has(s)}
              onClick={() => toggleStatus(s)}
            >
              <TaskStatusDot status={s} />
              {TASK_STATUS_LABEL[s]}
              <span className="review-chip-count">{statusCounts.get(s)}</span>
            </button>
          ))}
        </div>
        <div className="review-selects">
          <select aria-label="Workspace" value={workspaceId} onChange={(e) => setWorkspaceId(e.target.value)}>
            <option value="">All workspaces</option>
            {workspaceIds.map((id) => (
              <option key={id} value={id}>
                {wsName(id) ?? "Unknown workspace"}
              </option>
            ))}
          </select>
          <select aria-label="Age" value={ageDays} onChange={(e) => setAgeDays(Number(e.target.value))}>
            {AGE_CHOICES.map((a) => (
              <option key={a.days} value={a.days}>
                {a.label}
              </option>
            ))}
          </select>
          <select
            aria-label="Order"
            value={oldestFirst ? "old" : "new"}
            onChange={(e) => setOldestFirst(e.target.value === "old")}
          >
            <option value="old">Oldest first</option>
            <option value="new">Newest first</option>
          </select>
        </div>
      </div>

      <label className="review-selectall">
        <input
          type="checkbox"
          checked={allSelected}
          ref={(el) => {
            if (el) el.indeterminate = selectedShown.length > 0 && !allSelected;
          }}
          disabled={shown.length === 0 || busy}
          onChange={toggleAll}
        />
        <span>
          {selectedShown.length > 0
            ? `${selectedShown.length} of ${shown.length} selected`
            : `Select all ${shown.length} shown`}
        </span>
      </label>

      <div
        className="review-list"
        ref={listRef}
        onScroll={(e) => {
          scrollTop.current = e.currentTarget.scrollTop;
        }}
      >
        {state.bosunTasks === null ? (
          <div className="task-empty">Loading…</div>
        ) : shown.length === 0 ? (
          <div className="task-empty">Nothing matches. Clear a filter to see more.</div>
        ) : (
          shown.map((t) => {
            const ticket = orbitTicketLabel(t);
            const meta = [
              t.client?.trim(),
              wsName(t.workspaceId),
              t.source?.kind,
              ticket,
              `touched ${ageLabel(t.updatedAt)}`,
            ].filter(Boolean);
            const checked = selected.has(t.id);
            return (
              // The whole row is the checkbox: on a phone the box alone was too
              // small to hit, and a near miss opened the task instead. Opening
              // is its own button at the right edge.
              <div key={t.id} className={`review-row${checked ? " checked" : ""}`}>
                <label className="review-pick">
                  <input
                    type="checkbox"
                    className="review-box"
                    checked={checked}
                    disabled={busy}
                    onChange={() => toggle(t.id)}
                    aria-label={`Select ${displayTitle(t)}`}
                  />
                  <span className="review-main">
                    <span className="review-title">{displayTitle(t) || "Untitled to-do"}</span>
                    <span className="review-meta">
                      <span className={`review-status s-${t.status}`}>
                        <TaskStatusDot status={t.status} />
                        {TASK_STATUS_LABEL[t.status]}
                      </span>
                      {meta.map((m, i) => (
                        <span key={i} className="review-meta-part">
                          {m}
                        </span>
                      ))}
                    </span>
                    {t.nextStep?.trim() && <span className="review-next">{t.nextStep.trim()}</span>}
                  </span>
                </label>
                <button
                  type="button"
                  className="review-open"
                  aria-label={`Open ${displayTitle(t)}`}
                  title="Open this to-do"
                  onClick={() => openRow(t.id)}
                >
                  Open <ChevronIcon size={11} className="review-open-chev" />
                </button>
              </div>
            );
          })
        )}
      </div>

      {(selectedShown.length > 0 || progress || error || held.length > 0) && (
        <div className="review-bar" role="toolbar" aria-label="Selected to-dos">
          {progress ? (
            <span className="review-progress">
              {progress.verb} {progress.done}/{progress.total}…
            </span>
          ) : (
            <>
              <span className="review-bar-count">{selectedShown.length} selected</span>
              <button
                type="button"
                className="btn tone-allow"
                disabled={selectedShown.length === 0}
                onClick={() => void close(selectedShown, "done")}
              >
                <CheckIcon size={13} /> Done
              </button>
              <button
                type="button"
                className="btn tone-deny"
                disabled={selectedShown.length === 0}
                onClick={() => void close(selectedShown, "dropped")}
              >
                Drop
              </button>
              <button type="button" className="btn tone-deny" onClick={() => setSelected(new Set())}>
                Clear
              </button>
            </>
          )}
          {held.length > 0 && !progress && (
            <div className="review-held">
              <span>
                {held.length} client ticket{held.length === 1 ? " is" : "s are"} still open in Orbit (
                {held.slice(0, 6).map((h) => h.ticket).join(", ")}
                {held.length > 6 ? "…" : ""}). Closing may email the client.
              </span>
              <button type="button" className="btn bosun-small-btn" disabled={closingHeld} onClick={() => void closeHeld()}>
                {closingHeld ? "Closing…" : "Close in Orbit"}
              </button>
              <button type="button" className="btn bosun-small-btn" disabled={closingHeld} onClick={() => setHeld([])}>
                Leave open
              </button>
            </div>
          )}
          {error && <div className="task-error review-error">{error}</div>}
        </div>
      )}
    </section>
  );
}
