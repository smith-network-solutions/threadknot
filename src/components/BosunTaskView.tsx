import { useEffect, useMemo, useState } from "react";
import type { BosunTask, TaskStatus } from "../lib/protocol";
import {
  PICKABLE_STATUSES,
  TASK_STATUS_LABEL,
  dueLabel,
  displayTitle,
  isClosedStatus,
  isManualTask,
  localTime,
  dayLabel,
  localDayKey,
  snoozeLabel,
  snoozePresets,
  snoozeUntilDate,
} from "../lib/bosun";
import { copyText, elidePathMiddle } from "../lib/format";
import { allWorkspaces, findThread, useStore } from "../state/store";
import { BosunMark, useBosunImage } from "./BosunMark";
import { ClockIcon, PanelLeftIcon, PencilIcon, PlayIcon, XIcon } from "./icons";

/** The status as a coloured dot; the palette lives in styles.css
 *  (`.task-dot.s-<status>`), shared with the sidebar rows. */
export function TaskStatusDot({ status }: { status: TaskStatus }) {
  return <span className={`task-dot s-${status}`} aria-hidden="true" />;
}

/** "#2451" for a linked Orbit ticket (its number, else its id). */
export function orbitTicketLabel(task: Pick<BosunTask, "orbit">): string | null {
  const o = task.orbit;
  if (!o) return null;
  const n = (o.ticketNumber ?? "").trim() || o.taskId;
  return n.startsWith("#") ? n : `#${n}`;
}

/** The main pane when a task is open (docs/BOSUN.md §10.7): what the item is,
 *  where it stands, and the few things a person does with it. The task's
 *  thread is one click away, never shown in place of it. */
export function BosunTaskView() {
  const { state, dispatch, actions } = useStore();
  const taskId = state.activeTaskId;
  const task = useMemo(
    () => (taskId ? (state.bosunTasks ?? []).find((t) => t.id === taskId) ?? null : null),
    [taskId, state.bosunTasks],
  );
  const bosun = task ? state.bosuns.find((b) => b.id === task.bosunId) : undefined;
  const image = useBosunImage(task?.bosunId);
  const [editing, setEditing] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [snoozeOpen, setSnoozeOpen] = useState(false);
  const [snoozeDate, setSnoozeDate] = useState("");
  const [copied, setCopied] = useState(false);

  // A different task resets the transient bits.
  useEffect(() => {
    setEditing(false);
    setError(null);
    setSnoozeOpen(false);
    setSnoozeDate("");
  }, [taskId]);

  const header = (title: React.ReactNode, extra?: React.ReactNode) => (
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
      <div className="thread-head-main">{title}</div>
      {extra}
      <button
        type="button"
        className="icon-btn"
        aria-label="Close to-do"
        title="Close"
        onClick={() => actions.openTask(null)}
      >
        <XIcon size={15} />
      </button>
    </header>
  );

  if (!task) {
    return (
      <section className="thread-pane task-pane">
        {header(<h1 className="thread-title"><span className="thread-name">To-do</span></h1>)}
        <div className="task-body">
          <div className="task-empty">
            {state.bosunTasks === null ? "Loading…" : "This to-do is gone. It may have been deleted or pruned."}
          </div>
        </div>
      </section>
    );
  }

  const t = task;
  const now = new Date();
  const workspaceName = t.workspaceId
    ? (allWorkspaces(state).find((w) => w.id === t.workspaceId)?.name ?? null)
    : null;
  const thread = t.threadId ? findThread(state, t.threadId) : undefined;
  const closed = isClosedStatus(t.status);
  const ticket = orbitTicketLabel(t);

  async function run(label: string, fn: () => Promise<unknown>) {
    setBusy(label);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  const setStatus = (status: TaskStatus) =>
    status === t.status ? undefined : void run("status", () => actions.updateTask({ taskId: t.id, status }));

  const snooze = (until: string) =>
    void run("snooze", async () => {
      await actions.updateTask({ taskId: t.id, status: "snoozed", snoozedUntil: until });
      setSnoozeOpen(false);
    });

  function commitTitle() {
    setEditing(false);
    const next = titleDraft.trim();
    if (next && next !== t.title) void run("title", () => actions.updateTask({ taskId: t.id, title: next }));
  }

  const history = [...t.history].reverse();
  const presets = snoozePresets(now);

  return (
    <section className="thread-pane task-pane">
      {header(
        editing ? (
          <input
            className="title-input"
            autoFocus
            value={titleDraft}
            aria-label="To-do title"
            onChange={(e) => setTitleDraft(e.target.value)}
            onBlur={commitTitle}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
              if (e.key === "Escape") setEditing(false);
            }}
          />
        ) : (
          <>
            <h1 className="thread-title">
              <TaskStatusDot status={t.status} />
              <span className="thread-name" title={displayTitle(t)}>
                {displayTitle(t) || "Untitled to-do"}
              </span>
              <button
                className="icon-btn title-edit"
                aria-label="Rename to-do"
                onClick={() => {
                  setTitleDraft(displayTitle(t));
                  setEditing(true);
                }}
              >
                <PencilIcon size={13} />
              </button>
            </h1>
            <div className="task-head-sub">
              <BosunMark image={image} size={12} />
              <span>{bosun?.name ?? "Bosun"}</span>
              <span className="task-head-sep">·</span>
              <span className={`task-status-text s-${t.status}`}>{TASK_STATUS_LABEL[t.status]}</span>
              {t.status === "snoozed" && t.snoozedUntil && (
                <span className="task-head-muted">until {snoozeLabel(t.snoozedUntil, now)}</span>
              )}
            </div>
          </>
        ),
      )}
      <div className="task-body">
        <div className="task-inner">
          <div className="task-actions">
            {t.threadId ? (
              <button
                type="button"
                className="btn tone-allow task-primary"
                disabled={!!busy}
                onClick={() => void run("open", () => actions.selectThread(t.threadId!))}
                title={thread ? undefined : "The thread is not loaded here yet; opening asks its machine"}
              >
                Open conversation
              </button>
            ) : (
              <button
                type="button"
                className="btn tone-allow task-primary"
                disabled={!!busy}
                onClick={() => void run("work", () => actions.workTask(t.id))}
              >
                <PlayIcon size={11} /> {busy === "work" ? "Starting…" : "Work on it"}
              </button>
            )}
            {!closed && (
              <button
                type="button"
                className={`btn tone-deny${snoozeOpen ? " on" : ""}`}
                aria-expanded={snoozeOpen}
                disabled={!!busy}
                onClick={() => setSnoozeOpen((v) => !v)}
              >
                <ClockIcon size={12} /> Snooze
              </button>
            )}
            {t.status !== "done" && (
              <button type="button" className="btn tone-deny" disabled={!!busy} onClick={() => setStatus("done")}>
                Done
              </button>
            )}
            {t.status !== "dropped" && (
              <button
                type="button"
                className="btn tone-deny"
                disabled={!!busy}
                onClick={() => {
                  if (window.confirm("Drop this to-do? It moves to Done and stops being tracked.")) setStatus("dropped");
                }}
              >
                Drop
              </button>
            )}
            {isManualTask(t) && (
              <button
                type="button"
                className="btn tone-deny task-delete"
                disabled={!!busy}
                onClick={() => {
                  if (!window.confirm("Delete this to-do for good?")) return;
                  void run("delete", () => actions.deleteTask(t.id));
                }}
              >
                Delete
              </button>
            )}
          </div>
          {snoozeOpen && (
            <div className="task-snooze">
              {presets.map((p) => (
                <button key={p.id} type="button" className="task-pill" disabled={!!busy} onClick={() => snooze(p.until)}>
                  {p.label}
                </button>
              ))}
              <span className="task-snooze-date">
                <input
                  type="date"
                  aria-label="Snooze until date"
                  value={snoozeDate}
                  min={localDayKey(new Date(now.getTime() + 86_400_000).toISOString())}
                  onChange={(e) => setSnoozeDate(e.target.value)}
                />
                <button
                  type="button"
                  className="task-pill"
                  disabled={!!busy || !snoozeUntilDate(snoozeDate)}
                  onClick={() => {
                    const until = snoozeUntilDate(snoozeDate);
                    if (until) snooze(until);
                  }}
                >
                  Snooze to date
                </button>
              </span>
            </div>
          )}
          {error && <div className="task-error">{error}</div>}

          <div className="task-section">
            <span className="sched-label">Status</span>
            <div className="task-status-pills" role="radiogroup" aria-label="Status">
              {PICKABLE_STATUSES.map((s) => (
                <button
                  key={s}
                  type="button"
                  role="radio"
                  aria-checked={t.status === s}
                  className={`task-pill status-pill s-${s}${t.status === s ? " on" : ""}`}
                  disabled={!!busy}
                  onClick={() => setStatus(s)}
                >
                  <TaskStatusDot status={s} />
                  {TASK_STATUS_LABEL[s]}
                </button>
              ))}
              {(t.status === "snoozed" || t.status === "dropped") && (
                <span className={`task-pill status-pill s-${t.status} on`} aria-checked role="radio">
                  <TaskStatusDot status={t.status} />
                  {TASK_STATUS_LABEL[t.status]}
                </span>
              )}
            </div>
          </div>

          {t.summary.trim() && (
            <div className="task-section">
              <span className="sched-label">Summary</span>
              <p className="task-text">{t.summary}</p>
            </div>
          )}
          {t.nextStep?.trim() && (
            <div className="task-section">
              <span className="sched-label">Next step</span>
              <p className="task-text task-next">{t.nextStep}</p>
            </div>
          )}
          {t.pendingUpdates.length > 0 && (
            <div className="task-note">
              {t.pendingUpdates.length} new update{t.pendingUpdates.length === 1 ? "" : "s"} waiting for the
              conversation to go idle.
            </div>
          )}

          <dl className="task-facts">
            {t.due && (
              <>
                <dt>Due</dt>
                <dd>{dueLabel(t.due, now).replace(/^due /, "")}</dd>
              </>
            )}
            {t.client?.trim() && (
              <>
                <dt>Client</dt>
                <dd>{t.client}</dd>
              </>
            )}
            {workspaceName && (
              <>
                <dt>Workspace</dt>
                <dd>{workspaceName}</dd>
              </>
            )}
            {t.orbit && (
              <>
                <dt>Orbit</dt>
                <dd>
                  <a className="task-link" href={t.orbit.url} target="_blank" rel="noreferrer" title={t.orbit.url}>
                    {ticket} · {t.orbit.org}
                  </a>
                </dd>
              </>
            )}
            <dt>Source</dt>
            <dd className="task-source">
              <span className="task-source-kind">{t.source.kind || "unknown"}</span>
              {t.source.url ? (
                <a className="task-link" href={t.source.url} target="_blank" rel="noreferrer" title={t.source.url}>
                  {t.source.url}
                </a>
              ) : t.source.path ? (
                <button
                  type="button"
                  className="task-link"
                  title={copied ? "Copied" : `Copy ${t.source.path}`}
                  onClick={() =>
                    void copyText(t.source.path!).then((ok) => {
                      if (!ok) return;
                      setCopied(true);
                      window.setTimeout(() => setCopied(false), 1400);
                    })
                  }
                >
                  {copied ? "copied" : elidePathMiddle(t.source.path)}
                </button>
              ) : null}
            </dd>
            <dt>Opened</dt>
            <dd>
              {dayLabel(localDayKey(t.createdAt), now)} {localTime(t.createdAt)}
            </dd>
          </dl>

          {history.length > 0 && (
            <div className="task-section">
              <span className="sched-label">History</span>
              <ol className="task-history">
                {history.map((ev, i) => (
                  <li key={`${ev.at}-${i}`} className="task-event">
                    <div className="task-event-head">
                      <span className="task-event-when">
                        {dayLabel(localDayKey(ev.at), now)} {localTime(ev.at)}
                      </span>
                      <span className={`task-event-by by-${ev.by}`}>{ev.by === "you" ? "You" : ev.by === "bosun" ? (bosun?.name ?? "Bosun") : ev.by}</span>
                      {ev.status && (
                        <span className={`task-event-status s-${ev.status}`}>
                          <TaskStatusDot status={ev.status} />
                          {TASK_STATUS_LABEL[ev.status]}
                        </span>
                      )}
                    </div>
                    {ev.note.trim() && <div className="task-event-note">{ev.note}</div>}
                  </li>
                ))}
              </ol>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
