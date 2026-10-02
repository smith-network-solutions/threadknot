import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { BosunTask, PersistedEvent, TaskStatus } from "../lib/protocol";
import {
  PICKABLE_STATUSES,
  TASK_STATUS_LABEL,
  ageLabel,
  dueLabel,
  displayTitle,
  isClosedStatus,
  isManualTask,
  lastAssistantMessage,
  localTime,
  dayLabel,
  localDayKey,
  pendingQuestion,
  snoozeLabel,
  snoozePresets,
  snoozeUntilDate,
  visibleHistory,
} from "../lib/bosun";
import { copyText, elidePathMiddle } from "../lib/format";
import type { FeedItem } from "../state/feed";
import { allWorkspaces, findThread, useStore } from "../state/store";
import { BosunMark, useBosunImage } from "./BosunMark";
import { ContextMenu, type CtxItem } from "./ContextMenu";
import { ChevronIcon, MoreIcon, PanelLeftIcon, PencilIcon, PlayIcon, XIcon } from "./icons";
import { Markdown } from "./Markdown";
import { QuestionCard } from "./QuestionCard";

/** The status as a coloured dot; the palette lives in styles.css
 *  (`.task-dot.s-<status>`), shared with the sidebar rows. */
export function TaskStatusDot({ status }: { status: TaskStatus }) {
  return <span className={`task-dot s-${status}`} aria-hidden="true" />;
}

/** "#2451" for a linked Orbit ticket, or "Orbit" when it has no number (a
 *  raw task id is noise in a row and crowds the title out). */
export function orbitTicketLabel(task: Pick<BosunTask, "orbit">): string | null {
  const o = task.orbit;
  if (!o) return null;
  const n = (o.ticketNumber ?? "").trim();
  if (!n) return "Orbit";
  return n.startsWith("#") ? n : `#${n}`;
}

type QItem = Extract<FeedItem, { type: "question" }>;
type Menu = { kind: "status" | "more"; x: number; y: number };

/** How many events of the thread's tail the page reads. Enough to reach back
 *  past a long tool run to the last thing the agent said. */
const TAIL_LIMIT = 150;

/** The main pane when a task is open (docs/BOSUN.md §10.7), ordered for a
 *  phone: what the item is, where it stands, what Bosun is asking, what it
 *  said last, then the few things a person does with it. The thread is one
 *  tap away, never shown in place of the task. */
export function BosunTaskView() {
  const { state, dispatch, actions } = useStore();
  const taskId = state.activeTaskId;
  const task = useMemo(
    () => (taskId ? (state.bosunTasks ?? []).find((t) => t.id === taskId) ?? null : null),
    [taskId, state.bosunTasks],
  );
  const bosun = task ? state.bosuns.find((b) => b.id === task.bosunId) : undefined;
  const bosunName = bosun?.name ?? "Bosun";
  const image = useBosunImage(task?.bosunId);
  const thread = task?.threadId ? findThread(state, task.threadId) : undefined;

  const [editing, setEditing] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [dateOpen, setDateOpen] = useState(false);
  const [snoozeDate, setSnoozeDate] = useState("");
  const [copied, setCopied] = useState(false);
  const [orders, setOrders] = useState("");
  const [detailsOpen, setDetailsOpen] = useState(false);
  // The thread's tail, for "Latest from" and the pending question.
  const [tail, setTail] = useState<{ threadId: string; events: PersistedEvent[] } | null>(null);
  const [tailError, setTailError] = useState(false);
  const [tailTick, setTailTick] = useState(0);
  const tailSeq = useRef(0);
  // The inline answer: local selection, in-flight flag, and what was sent
  // (kept after the thread moves on, so the card can say "Answered: …").
  const [qAnswers, setQAnswers] = useState<Record<string, string[]>>({});
  const [qPending, setQPending] = useState(false);
  const [answered, setAnswered] = useState<{ requestId: string; answers: Record<string, string[]> } | null>(null);
  const [qError, setQError] = useState<string | null>(null);

  // A different task resets the transient bits.
  useEffect(() => {
    setEditing(false);
    setError(null);
    setMenu(null);
    setDateOpen(false);
    setSnoozeDate("");
    setOrders("");
    setDetailsOpen(false);
    setTail(null);
    setTailError(false);
    setQAnswers({});
    setQPending(false);
    setAnswered(null);
    setQError(null);
  }, [taskId]);

  // Read the thread's tail on open, and again whenever the thread moves
  // (status or updatedAt in state), so the page follows the conversation.
  const tailThreadId = task?.threadId ?? null;
  useEffect(() => {
    if (!tailThreadId) return;
    const seq = ++tailSeq.current;
    actions
      .threadTail(tailThreadId, TAIL_LIMIT)
      .then(({ events }) => {
        if (seq !== tailSeq.current) return;
        setTail({ threadId: tailThreadId, events });
        setTailError(false);
      })
      .catch(() => {
        if (seq === tailSeq.current) setTailError(true);
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tailThreadId, thread?.status, thread?.updatedAt, tailTick]);

  const events = tail && tail.threadId === tailThreadId ? tail.events : null;
  const latest = useMemo(() => (events ? lastAssistantMessage(events) : null), [events]);
  const question = useMemo(() => (events ? pendingQuestion(events) : null), [events]);

  // The menu closes itself on the trigger's mousedown; remember which one and
  // when, so the click that follows doesn't reopen it.
  const menuClosed = useRef<{ kind: Menu["kind"]; at: number } | null>(null);
  const closeMenu = useCallback(() => {
    setMenu((m) => {
      if (m) menuClosed.current = { kind: m.kind, at: Date.now() };
      return null;
    });
  }, []);

  const header = (main: React.ReactNode) => (
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
      <div className="thread-head-main task-head-main">{main}</div>
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
        {header(<span className="task-head-who">To-do</span>)}
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
  const closed = isClosedStatus(t.status);
  const ticket = orbitTicketLabel(t);
  const threadBusy = thread?.status === "running";

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

  const sendOrders = () => {
    const text = orders.trim();
    if (!text) return;
    void run("orders", async () => {
      await actions.workTask(t.id, text);
      setOrders("");
    });
  };

  const setStatus = (status: TaskStatus) =>
    status === t.status ? undefined : void run("status", () => actions.updateTask({ taskId: t.id, status }));

  const snooze = (until: string) =>
    void run("snooze", async () => {
      await actions.updateTask({ taskId: t.id, status: "snoozed", snoozedUntil: until });
      setDateOpen(false);
    });

  const drop = () => {
    if (window.confirm("Drop this to-do? It moves to Done and stops being tracked.")) setStatus("dropped");
  };

  const remove = () => {
    if (!window.confirm("Delete this to-do for good?")) return;
    void run("delete", () => actions.deleteTask(t.id));
  };

  function commitTitle() {
    setEditing(false);
    const next = titleDraft.trim();
    if (next && next !== t.title) void run("title", () => actions.updateTask({ taskId: t.id, title: next }));
  }

  const openMenu = (kind: Menu["kind"]) => (e: React.MouseEvent<HTMLButtonElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const last = menuClosed.current;
    if (last && last.kind === kind && Date.now() - last.at < 400) return;
    setMenu((m) => (m?.kind === kind ? null : { kind, x: kind === "more" ? r.right - 200 : r.left, y: r.bottom + 4 }));
  };

  const statusItems: CtxItem[] = PICKABLE_STATUSES.map((s) => ({
    label: TASK_STATUS_LABEL[s],
    icon: <TaskStatusDot status={s} />,
    badge: s === t.status ? "current" : undefined,
    disabled: !!busy,
    onSelect: () => setStatus(s),
  }));

  const presets = snoozePresets(now);
  const moreItems: CtxItem[] = [];
  if (!closed) {
    moreItems.push({ label: "Snooze", kind: "heading", onSelect: () => undefined });
    for (const p of presets) {
      moreItems.push({ label: p.label, inset: true, disabled: !!busy, onSelect: () => snooze(p.until) });
    }
    moreItems.push({
      label: "Pick a date…",
      inset: true,
      disabled: !!busy,
      onSelect: () => setDateOpen(true),
    });
  }
  if (t.status !== "dropped") {
    moreItems.push({ label: "Drop", dividerBefore: !closed, disabled: !!busy, onSelect: drop });
  }
  if (isManualTask(t)) {
    moreItems.push({ label: "Delete", danger: true, disabled: !!busy, onSelect: remove });
  }

  // The meta line under the title: client · workspace · due · #ticket · age.
  const due = t.due ? dueLabel(t.due, now) : null;
  const overdue = !!due && due.startsWith("overdue") && !closed;
  const meta: React.ReactNode[] = [];
  if (t.client?.trim()) meta.push(<span key="client" className="task-meta-client">{t.client.trim()}</span>);
  if (workspaceName) meta.push(<span key="ws">{workspaceName}</span>);
  if (t.status === "snoozed" && t.snoozedUntil) meta.push(<span key="snz">until {snoozeLabel(t.snoozedUntil, now)}</span>);
  if (due) meta.push(<span key="due" className={overdue ? "task-meta-overdue" : undefined}>{due}</span>);
  if (t.orbit && ticket) {
    meta.push(
      <a key="orbit" className="task-meta-link" href={t.orbit.url} target="_blank" rel="noreferrer" title={`Open in Orbit (${t.orbit.org})`}>
        Orbit {ticket}
      </a>,
    );
  }
  meta.push(<span key="age">opened {ageLabel(t.createdAt, now)}</span>);

  const summary = t.summary.trim();
  const nextStep = t.nextStep?.trim() ?? "";
  const history = [...visibleHistory(t.history)].reverse();

  // The question card: what the thread is waiting on, or what was just sent.
  const showAnswered = answered && (!question || question.requestId === answered.requestId);
  const askQuestion = showAnswered ? null : question;
  const qItem: QItem | null = askQuestion
    ? {
        id: `task-q-${askQuestion.requestId}`,
        type: "question",
        requestId: askQuestion.requestId,
        questions: askQuestion.questions,
        answers: qAnswers,
        answered: false,
        pending: qPending,
      }
    : null;
  const answerQuestion = async (requestId: string, answers: Record<string, string[]>) => {
    if (!t.threadId) return;
    setQPending(true);
    setQError(null);
    try {
      await actions.answerThreadQuestion(t.threadId, requestId, answers);
      setAnswered({ requestId, answers });
      // The engine moves the task off Needs you; pull both soon after.
      window.setTimeout(() => {
        setTailTick((n) => n + 1);
        void actions.refreshBosunTasks().catch(() => undefined);
      }, 800);
    } catch (e) {
      setQError(e instanceof Error ? e.message : String(e));
    } finally {
      setQPending(false);
    }
  };
  const answeredText = answered
    ? Object.values(answered.answers).flat().filter(Boolean).join(", ")
    : "";

  return (
    <section className="thread-pane task-pane">
      {header(
        <div className="task-head-row">
          <span className="task-head-who">
            <BosunMark image={image} size={14} />
            <span>{bosunName}</span>
          </span>
          <button
            type="button"
            className={`task-status-chip s-${t.status}`}
            aria-haspopup="menu"
            aria-expanded={menu?.kind === "status"}
            disabled={!!busy}
            onClick={openMenu("status")}
            title="Change status"
          >
            <TaskStatusDot status={t.status} />
            {TASK_STATUS_LABEL[t.status]}
            <ChevronIcon size={11} open className="task-status-caret" />
          </button>
        </div>,
      )}
      <div className="task-body">
        <div className="task-inner">
          <div className="task-title-block">
            {editing ? (
              <textarea
                className="task-title-input"
                autoFocus
                rows={2}
                value={titleDraft}
                aria-label="To-do title"
                onChange={(e) => setTitleDraft(e.target.value)}
                onBlur={commitTitle}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    (e.target as HTMLTextAreaElement).blur();
                  }
                  if (e.key === "Escape") setEditing(false);
                }}
              />
            ) : (
              <h1 className="task-title">
                <span>{displayTitle(t) || "Untitled to-do"}</span>
                <button
                  className="icon-btn task-title-edit"
                  aria-label="Rename to-do"
                  title="Rename"
                  onClick={() => {
                    setTitleDraft(displayTitle(t));
                    setEditing(true);
                  }}
                >
                  <PencilIcon size={14} />
                </button>
              </h1>
            )}
            <div className="task-meta">
              {meta.map((m, i) => (
                <span key={i} className="task-meta-part">
                  {i > 0 && <span className="task-meta-sep" aria-hidden="true">·</span>}
                  {m}
                </span>
              ))}
            </div>
          </div>

          {summary || nextStep ? (
            <div className="task-card task-stands">
              <span className="task-card-label">Where it stands</span>
              {summary && <p className="task-text">{summary}</p>}
              {nextStep && (
                <p className="task-text task-next">
                  <strong>Next:</strong> {nextStep}
                </p>
              )}
            </div>
          ) : !t.threadId ? (
            <p className="task-quiet">
              No summary yet. <em>Work on it</em> and {bosunName} will look into it.
            </p>
          ) : null}
          {t.pendingUpdates.length > 0 && (
            <div className="task-note">
              {t.pendingUpdates.length} new update{t.pendingUpdates.length === 1 ? "" : "s"} waiting for the
              conversation to go idle.
            </div>
          )}

          {(qItem || showAnswered) && (
            <div className="task-card task-asking">
              <span className="task-card-label">{bosunName} is asking</span>
              {qItem ? (
                <QuestionCard
                  key={qItem.requestId}
                  item={qItem}
                  onSetAnswers={(_id, a) => setQAnswers(a)}
                  onRespond={answerQuestion}
                />
              ) : (
                <div className="question-resolved">
                  <span>Answered{answeredText ? `: ${answeredText}` : ""}</span>
                </div>
              )}
              {qError && <div className="task-error">{qError}</div>}
            </div>
          )}

          {t.threadId && (
            <LatestMessage
              key={t.threadId}
              bosunName={bosunName}
              latest={latest}
              loading={!events && !tailError}
              failed={tailError && !events}
              now={now}
            />
          )}

          <div className="task-actions">
            <button
              type="button"
              className="btn tone-allow task-primary"
              disabled={!!busy || threadBusy}
              onClick={() => void run("work", () => actions.workTask(t.id))}
              title={threadBusy ? `${bosunName} is already working on this` : `${bosunName} picks this up and takes it as far as the standing orders allow`}
            >
              <PlayIcon size={11} /> {busy === "work" ? "Starting…" : threadBusy ? "Working…" : "Work on it"}
            </button>
            <div className="task-actions-rest">
              {t.threadId && (
                <button
                  type="button"
                  className="btn tone-deny"
                  disabled={!!busy}
                  onClick={() => void run("open", () => actions.selectThread(t.threadId!))}
                  title={thread ? undefined : "The thread is not loaded here yet; opening asks its machine"}
                >
                  Open conversation
                </button>
              )}
              {t.status !== "done" && (
                <button type="button" className="btn tone-deny" disabled={!!busy} onClick={() => setStatus("done")}>
                  Done
                </button>
              )}
              {moreItems.length > 0 && (
                <button
                  type="button"
                  className="btn tone-deny task-more"
                  aria-label="More actions"
                  title="Snooze, drop, delete"
                  aria-haspopup="menu"
                  aria-expanded={menu?.kind === "more"}
                  disabled={!!busy}
                  onClick={openMenu("more")}
                >
                  <MoreIcon size={15} />
                </button>
              )}
            </div>
          </div>
          {dateOpen && !closed && (
            <div className="task-snooze">
              <span className="task-snooze-label">Snooze until</span>
              <input
                type="date"
                aria-label="Snooze until date"
                value={snoozeDate}
                min={localDayKey(new Date(now.getTime() + 86_400_000).toISOString())}
                onChange={(e) => setSnoozeDate(e.target.value)}
              />
              <button
                type="button"
                className="btn tone-allow"
                disabled={!!busy || !snoozeUntilDate(snoozeDate)}
                onClick={() => {
                  const until = snoozeUntilDate(snoozeDate);
                  if (until) snooze(until);
                }}
              >
                Snooze
              </button>
              <button type="button" className="btn tone-deny" onClick={() => setDateOpen(false)}>
                Cancel
              </button>
            </div>
          )}
          {error && <div className="task-error">{error}</div>}

          <form
            className="task-orders"
            onSubmit={(e) => {
              e.preventDefault();
              sendOrders();
            }}
          >
            <textarea
              className="task-orders-input"
              rows={2}
              placeholder={`Tell ${bosunName} what to do with this, e.g. "draft the reply and close the ticket"`}
              value={orders}
              disabled={!!busy || threadBusy}
              onChange={(e) => setOrders(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  sendOrders();
                }
              }}
            />
            <button type="submit" className="btn tone-allow" disabled={!orders.trim() || !!busy || threadBusy}>
              {busy === "orders" ? "Sending…" : "Send"}
            </button>
          </form>

          <div className={`task-details${detailsOpen ? " open" : ""}`}>
            <button
              type="button"
              className="task-details-toggle"
              aria-expanded={detailsOpen}
              onClick={() => setDetailsOpen((v) => !v)}
            >
              <ChevronIcon size={12} open={detailsOpen} className="row-chevron" />
              Details
              {!detailsOpen && history.length > 0 && (
                <span className="task-details-count">
                  {history.length} history {history.length === 1 ? "entry" : "entries"}
                </span>
              )}
            </button>
            {detailsOpen && (
              <div className="task-details-body">
                <dl className="task-facts">
                  {workspaceName && (
                    <>
                      <dt>Workspace</dt>
                      <dd>{workspaceName}</dd>
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
                  {t.orbit && (
                    <>
                      <dt>Orbit</dt>
                      <dd>
                        <a className="task-link" href={t.orbit.url} target="_blank" rel="noreferrer" title={t.orbit.url}>
                          {ticket} · {t.orbit.org} · {t.orbit.taskId}
                        </a>
                      </dd>
                    </>
                  )}
                  <dt>Key</dt>
                  <dd className="task-key">{t.key}</dd>
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
                            <span className={`task-event-by by-${ev.by}`}>
                              {ev.by === "you" ? "You" : ev.by === "bosun" ? bosunName : ev.by}
                            </span>
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
            )}
          </div>
        </div>
      </div>
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menu.kind === "status" ? statusItems : moreItems}
          title={menu.kind === "status" ? "Status" : undefined}
          onClose={closeMenu}
        />
      )}
    </section>
  );
}

/** "Latest from <bosun>": the thread's last assistant message through the
 *  feed's markdown renderer, clamped to ~10 lines until expanded. */
function LatestMessage({
  bosunName,
  latest,
  loading,
  failed,
  now,
}: {
  bosunName: string;
  latest: { text: string; ts: string } | null;
  loading: boolean;
  failed: boolean;
  now: Date;
}) {
  const [expanded, setExpanded] = useState(false);
  const [overflows, setOverflows] = useState(false);
  const bodyRef = useRef<HTMLDivElement>(null);
  const text = latest?.text ?? "";

  useEffect(() => setExpanded(false), [text]);

  // Only offer "Show more" when the clamp actually hides something.
  useLayoutEffect(() => {
    const el = bodyRef.current;
    if (!el || expanded) return;
    const measure = () => setOverflows(el.scrollHeight > el.clientHeight + 2);
    measure();
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : null;
    ro?.observe(el);
    return () => ro?.disconnect();
  }, [text, expanded]);

  if (!latest) {
    if (loading) return <p className="task-quiet">Reading the conversation…</p>;
    if (failed) return <p className="task-quiet">Couldn't load the conversation right now.</p>;
    return null;
  }

  return (
    <div className="task-card task-latest">
      <span className="task-card-label">
        Latest from {bosunName}
        <span className="task-card-when">{ageLabel(latest.ts, now)}</span>
      </span>
      <div ref={bodyRef} className={`task-latest-body${expanded ? " expanded" : ""}${overflows && !expanded ? " clamped" : ""}`}>
        <Markdown text={text} />
      </div>
      {(overflows || expanded) && (
        <button type="button" className="task-show-more" onClick={() => setExpanded((v) => !v)}>
          {expanded ? "Show less" : "Show more"}
        </button>
      )}
    </div>
  );
}
