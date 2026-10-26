import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { BosunTask, DayPlan, OrbitCloseOutcome, PlanBlock, PlanItem } from "../lib/protocol";
import {
  displayTitle,
  isClosedStatus,
  localDayKey,
  openTaskOrConversation,
  setTaskReturnPage,
  taskMatches,
} from "../lib/bosun";
import { useStore } from "../state/store";
import { orbitTicketLabel } from "./BosunTaskView";
import { CheckIcon, ChevronIcon, PanelLeftIcon, PlusIcon, TrashIcon, XIcon } from "./icons";
import "../styles/bosun-today.css";

const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function todayKey(): string {
  return localDayKey(new Date().toISOString());
}

function parseDay(key: string): Date {
  const [y, m, d] = key.split("-").map(Number);
  return new Date(y, m - 1, d);
}

function shiftDay(key: string, days: number): string {
  const d = parseDay(key);
  d.setDate(d.getDate() + days);
  return localDayKey(d.toISOString());
}

/** "Monday, Oct 26". */
function longDay(key: string): string {
  const d = parseDay(key);
  return `${WEEKDAYS[d.getDay()]}, ${MONTHS[d.getMonth()]} ${d.getDate()}`;
}

/** "Fri" for carry-over, "yesterday" when it is. */
function shortDay(key: string, from: string): string {
  if (key === shiftDay(from, -1)) return "yesterday";
  return WEEKDAYS[parseDay(key).getDay()].slice(0, 3);
}

/** "13:45" → "1:45 PM". */
function clock(hhmm: string): string {
  const [h, m] = hhmm.split(":").map(Number);
  const suffix = h >= 12 ? "PM" : "AM";
  return `${h % 12 === 0 ? 12 : h % 12}:${String(m).padStart(2, "0")} ${suffix}`;
}

function blockTime(b: PlanBlock): string | null {
  if (b.start && b.end) {
    const [s, e] = [clock(b.start), clock(b.end)];
    // "9:00 – 10:30 AM" when both halves share AM/PM.
    return s.slice(-2) === e.slice(-2) ? `${s.slice(0, -3)} – ${e}` : `${s} – ${e}`;
  }
  return b.start ? clock(b.start) : null;
}

function nowHHMM(): string {
  const d = new Date();
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function newId(): string {
  return `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
}

/** Add `mins` to "HH:MM", clamped to the day. */
function addMinutes(hhmm: string, mins: number): string {
  const [h, m] = hhmm.split(":").map(Number);
  const t = Math.min(23 * 60 + 59, h * 60 + m + mins);
  return `${String(Math.floor(t / 60)).padStart(2, "0")}:${String(t % 60).padStart(2, "0")}`;
}

type Confirm = { ticket: string; origin: string; busy?: boolean };
type Drag = { itemId: string; fromBlock: string } | null;

/** The Today page (docs/BOSUN.md §11): the day's to-dos in time blocks,
 *  worked down and ticked off. Every row is a Bosun task, so its checkbox
 *  closes the task and, through §10.8, its Orbit ticket; a client ticket asks
 *  first. Laid out for a phone first: whole-width rows, big checkboxes. */
export function BosunToday() {
  const { state, dispatch, actions } = useStore();
  const [date, setDate] = useState(todayKey);
  const [plan, setPlan] = useState<DayPlan | null>(null);
  const [lastDate, setLastDate] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; taskId?: string } | null>(null);
  const [confirms, setConfirms] = useState<Record<string, Confirm>>({});
  const [busyRows, setBusyRows] = useState<Record<string, boolean>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [adding, setAdding] = useState<string | null>(null);
  const [editingBlock, setEditingBlock] = useState<string | null>(null);
  const [trayOpen, setTrayOpen] = useState(false);
  const [drafting, setDrafting] = useState(false);
  const [drag, setDrag] = useState<Drag>(null);
  const [now, setNow] = useState(nowHHMM);
  const isToday = date === todayKey();

  // Saves are serialised and only the newest result is kept, so a quick run
  // of ticks never lands out of order.
  const saveSeq = useRef(0);
  const saving = useRef(0);

  const load = useCallback(
    async (d: string) => {
      try {
        const { plan, lastDate } = await actions.getPlan(d);
        if (saving.current === 0) setPlan(plan);
        setLastDate(lastDate ?? null);
        setError(null);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [actions],
  );

  useEffect(() => {
    setPlan(null);
    void load(date);
  }, [date, load]);

  // Another device, or the Bosun, changed a plan.
  const rev = useRef(state.bosunPlanRev);
  useEffect(() => {
    if (rev.current === state.bosunPlanRev) return;
    rev.current = state.bosunPlanRev;
    void load(date);
  }, [state.bosunPlanRev, date, load]);

  useEffect(() => {
    const t = window.setInterval(() => setNow(nowHHMM()), 60_000);
    return () => window.clearInterval(t);
  }, []);

  const commit = useCallback(
    (blocks: PlanBlock[]) => {
      setPlan((p) => (p ? { ...p, blocks } : { date, blocks }));
      const seq = ++saveSeq.current;
      saving.current += 1;
      actions
        .setPlan(date, blocks)
        .then((kept) => {
          if (seq === saveSeq.current) setPlan(kept);
        })
        .catch((e) => setError(e instanceof Error ? e.message : String(e)))
        .finally(() => {
          saving.current -= 1;
        });
    },
    [actions, date],
  );

  const tasks = state.bosunTasks ?? [];
  const byId = useMemo(() => new Map(tasks.map((t) => [t.id, t])), [tasks]);
  const blocks = plan?.blocks ?? [];
  const planned = useMemo(() => new Set(blocks.flatMap((b) => b.items.map((i) => i.taskId))), [blocks]);

  const isDone = useCallback(
    (item: PlanItem) => {
      const t = byId.get(item.taskId);
      return t ? isClosedStatus(t.status) : true;
    },
    [byId],
  );

  const rows = blocks.reduce((n, b) => n + b.items.length, 0);
  const doneRows = blocks.reduce((n, b) => n + b.items.filter(isDone).length, 0);
  const unplanned = useMemo(
    () =>
      tasks
        .filter((t) => !isClosedStatus(t.status) && !planned.has(t.id) && !t.key.startsWith("plan:"))
        .sort((a, b) => (a.status === "needs_you" ? -1 : 0) - (b.status === "needs_you" ? -1 : 0)),
    [tasks, planned],
  );

  // ---- block + row edits -------------------------------------------------

  const updateBlocks = (f: (blocks: PlanBlock[]) => PlanBlock[]) => commit(f(blocks.map((b) => ({ ...b, items: [...b.items] }))));

  function addBlock() {
    const last = blocks[blocks.length - 1];
    const start = last?.end ?? (isToday ? addMinutes(now, 30 - (Number(now.slice(3)) % 30)) : "09:00");
    updateBlocks((bs) => [
      ...bs,
      { id: newId(), title: "New block", start, end: addMinutes(start, 60), items: [] },
    ]);
    setEditingBlock(null);
  }

  function addRow(blockId: string, taskId: string) {
    if (planned.has(taskId)) return;
    updateBlocks((bs) =>
      bs.map((b) => (b.id === blockId ? { ...b, items: [...b.items, { id: newId(), taskId, steps: [] }] } : b)),
    );
  }

  function moveRow(itemId: string, toBlock: string, beforeItem?: string) {
    updateBlocks((bs) => {
      let moving: PlanItem | undefined;
      const out = bs.map((b) => {
        const i = b.items.findIndex((x) => x.id === itemId);
        if (i >= 0) {
          moving = b.items[i];
          return { ...b, items: b.items.filter((x) => x.id !== itemId) };
        }
        return b;
      });
      if (!moving) return bs;
      return out.map((b) => {
        if (b.id !== toBlock) return b;
        const at = beforeItem ? b.items.findIndex((x) => x.id === beforeItem) : -1;
        const items = [...b.items];
        items.splice(at < 0 ? items.length : at, 0, moving!);
        return { ...b, items };
      });
    });
  }

  function nudgeRow(blockId: string, itemId: string, delta: number) {
    updateBlocks((bs) =>
      bs.map((b) => {
        if (b.id !== blockId) return b;
        const i = b.items.findIndex((x) => x.id === itemId);
        const j = i + delta;
        if (i < 0 || j < 0 || j >= b.items.length) return b;
        const items = [...b.items];
        [items[i], items[j]] = [items[j], items[i]];
        return { ...b, items };
      }),
    );
  }

  function removeRow(itemId: string) {
    updateBlocks((bs) => bs.map((b) => ({ ...b, items: b.items.filter((x) => x.id !== itemId) })));
  }

  function editItem(itemId: string, f: (item: PlanItem) => PlanItem) {
    updateBlocks((bs) => bs.map((b) => ({ ...b, items: b.items.map((x) => (x.id === itemId ? f(x) : x)) })));
  }

  function editBlock(blockId: string, patch: Partial<PlanBlock>) {
    updateBlocks((bs) => bs.map((b) => (b.id === blockId ? { ...b, ...patch } : b)));
  }

  function deleteBlock(blockId: string) {
    const b = blocks.find((x) => x.id === blockId);
    if (b && b.items.length > 0 && !window.confirm(`Remove "${b.title}" and its ${b.items.length} row(s) from the plan? The to-dos stay open.`)) {
      return;
    }
    updateBlocks((bs) => bs.filter((x) => x.id !== blockId));
  }

  async function carryOver() {
    if (!lastDate) return;
    try {
      const { plan: prev } = await actions.getPlan(lastDate);
      const carried = prev.blocks
        .flatMap((b) => b.items)
        .filter((i) => {
          const t = byId.get(i.taskId);
          return t && !isClosedStatus(t.status) && !planned.has(i.taskId);
        })
        .map((i) => ({ ...i, id: newId(), steps: i.steps.map((s) => ({ ...s, id: newId() })) }));
      if (carried.length === 0) {
        setNotice({ text: `Everything from ${shortDay(lastDate, date)} is done or already planned.` });
        return;
      }
      updateBlocks((bs) => [
        { id: newId(), title: `Carried over from ${shortDay(lastDate, date)}`, items: carried },
        ...bs,
      ]);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function draft() {
    setDrafting(true);
    setError(null);
    try {
      const { taskId } = await actions.draftPlan(date);
      setNotice({
        text: `${state.bosuns[0]?.name ?? "Your Bosun"} is drafting the plan. It fills in here when it's done.`,
        taskId,
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setDrafting(false);
    }
  }

  // ---- ticking -----------------------------------------------------------

  async function toggleRow(item: PlanItem) {
    const task = byId.get(item.taskId);
    if (!task || busyRows[task.id]) return;
    setBusyRows((b) => ({ ...b, [task.id]: true }));
    setError(null);
    try {
      if (isClosedStatus(task.status)) {
        // Un-ticking reopens the to-do; Orbit is left alone.
        await actions.updateTask({ taskId: task.id, status: "todo", orbit: "skip" });
        setConfirms((c) => {
          const { [task.id]: _, ...rest } = c;
          return rest;
        });
      } else {
        const res = await actions.updateTask({ taskId: task.id, status: "done" });
        const oc: OrbitCloseOutcome | undefined = res.orbitClose;
        if (oc?.needsConfirm) {
          setConfirms((c) => ({
            ...c,
            [task.id]: { ticket: oc.ticket ? `#${oc.ticket}` : (orbitTicketLabel(task) ?? "ticket"), origin: oc.origin ?? "client" },
          }));
        } else if (oc?.error) {
          setError(`Done here, but Orbit ${orbitTicketLabel(task) ?? ""} is still open: ${oc.error}`);
        }
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusyRows((b) => ({ ...b, [task.id]: false }));
    }
  }

  async function confirmOrbit(taskId: string, close: boolean) {
    if (!close) {
      setConfirms((c) => {
        const { [taskId]: _, ...rest } = c;
        return rest;
      });
      return;
    }
    setConfirms((c) => ({ ...c, [taskId]: { ...c[taskId], busy: true } }));
    try {
      const oc = await actions.closeTaskInOrbit(taskId);
      if (!oc.closed) throw new Error(oc.error ?? "Orbit did not close it");
      setConfirms((c) => {
        const { [taskId]: _, ...rest } = c;
        return rest;
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setConfirms((c) => ({ ...c, [taskId]: { ...c[taskId], busy: false } }));
    }
  }

  function openRow(taskId: string) {
    const task = byId.get(taskId);
    if (task) {
      openTaskOrConversation(task, actions, "today");
    } else {
      setTaskReturnPage("today");
      actions.openTask(taskId);
    }
  }

  // ---- render ------------------------------------------------------------

  const pct = rows ? Math.round((doneRows / rows) * 100) : 0;

  return (
    <section className="thread-pane task-pane today-pane">
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
        <div className="thread-head-main task-head-main today-head-main">
          <button type="button" className="icon-btn today-day-nav" aria-label="Previous day" onClick={() => setDate((d) => shiftDay(d, -1))}>
            <ChevronIcon size={14} className="today-chev-left" />
          </button>
          <span className="task-head-who today-title">
            {isToday ? "Today" : longDay(date)}
            {isToday && <span className="today-date">{longDay(date)}</span>}
          </span>
          <button type="button" className="icon-btn today-day-nav" aria-label="Next day" onClick={() => setDate((d) => shiftDay(d, 1))}>
            <ChevronIcon size={14} />
          </button>
          {!isToday && (
            <button type="button" className="btn bosun-small-btn" onClick={() => setDate(todayKey())}>
              Today
            </button>
          )}
        </div>
        <button type="button" className="icon-btn" aria-label="Close Today" title="Close" onClick={() => actions.openTask(null)}>
          <XIcon size={15} />
        </button>
      </header>

      <div className="today-progress" aria-label={`${doneRows} of ${rows} done`}>
        <div className="today-progress-text">
          <strong>
            {doneRows} of {rows}
          </strong>{" "}
          done
        </div>
        <div className="today-progress-bar">
          <div className="today-progress-fill" style={{ width: `${pct}%` }} />
        </div>
        <div className="today-tools">
          <button type="button" className="btn bosun-small-btn" disabled={drafting || state.bosuns.length === 0} onClick={() => void draft()}>
            {drafting ? "Starting…" : rows ? "Re-plan with Bosun" : "Plan my day"}
          </button>
          {lastDate && (
            <button type="button" className="btn bosun-small-btn" onClick={() => void carryOver()}>
              Carry over from {shortDay(lastDate, date)}
            </button>
          )}
          <button type="button" className="btn bosun-small-btn" onClick={addBlock}>
            <PlusIcon size={11} /> Block
          </button>
        </div>
      </div>

      {notice && (
        <div className="today-notice">
          <span>{notice.text}</span>
          {notice.taskId && (
            <button type="button" className="today-link" onClick={() => openRow(notice.taskId!)}>
              Watch it
            </button>
          )}
          <button type="button" className="icon-btn" aria-label="Dismiss" onClick={() => setNotice(null)}>
            <XIcon size={12} />
          </button>
        </div>
      )}
      {error && (
        <div className="today-error" role="alert">
          <span>{error}</span>
          <button type="button" className="icon-btn" aria-label="Dismiss" onClick={() => setError(null)}>
            <XIcon size={12} />
          </button>
        </div>
      )}

      <div className="today-list">
        {plan === null ? (
          <div className="today-empty">Loading…</div>
        ) : blocks.length === 0 ? (
          <div className="today-empty">
            <p>Nothing planned for {isToday ? "today" : longDay(date)}.</p>
            <p className="today-empty-hint">
              Ask your Bosun to plan it, carry over what's unfinished, or add a block and drop to-dos in.
            </p>
          </div>
        ) : (
          blocks.map((b) => {
            const done = b.items.filter(isDone).length;
            const current = isToday && !!b.start && !!b.end && now >= b.start && now < b.end;
            const past = isToday && !!b.end && now >= b.end;
            const complete = b.items.length > 0 && done === b.items.length;
            return (
              <div
                key={b.id}
                className={`today-block${current ? " now" : ""}${complete ? " complete" : ""}${past && !complete ? " late" : ""}`}
                onDragOver={(e) => {
                  if (drag) e.preventDefault();
                }}
                onDrop={(e) => {
                  e.preventDefault();
                  if (drag) moveRow(drag.itemId, b.id);
                  setDrag(null);
                }}
              >
                <div className="today-block-head">
                  {editingBlock === b.id ? (
                    <BlockEditor
                      block={b}
                      onSave={(patch) => {
                        editBlock(b.id, patch);
                        setEditingBlock(null);
                      }}
                      onCancel={() => setEditingBlock(null)}
                      onDelete={() => {
                        setEditingBlock(null);
                        deleteBlock(b.id);
                      }}
                    />
                  ) : (
                    <button type="button" className="today-block-title" title="Edit block" onClick={() => setEditingBlock(b.id)}>
                      {blockTime(b) && <span className="today-block-time">{blockTime(b)}</span>}
                      <span className="today-block-name">{b.title || "Untitled block"}</span>
                    </button>
                  )}
                  {current && <span className="today-now-pill">Now</span>}
                  <span className="today-block-count">
                    {done}/{b.items.length}
                  </span>
                </div>

                {b.items.map((item) => (
                  <PlanRow
                    key={item.id}
                    item={item}
                    task={byId.get(item.taskId)}
                    busy={!!busyRows[item.taskId]}
                    confirm={confirms[item.taskId]}
                    expanded={!!expanded[item.id]}
                    blocks={blocks}
                    blockId={b.id}
                    first={b.items[0]?.id === item.id}
                    last={b.items[b.items.length - 1]?.id === item.id}
                    onToggle={() => void toggleRow(item)}
                    onOpen={() => openRow(item.taskId)}
                    onExpand={() => setExpanded((x) => ({ ...x, [item.id]: !x[item.id] }))}
                    onEdit={(f) => editItem(item.id, f)}
                    onMove={(to) => moveRow(item.id, to)}
                    onNudge={(d) => nudgeRow(b.id, item.id, d)}
                    onRemove={() => removeRow(item.id)}
                    onConfirm={(close) => void confirmOrbit(item.taskId, close)}
                    onDragStart={() => setDrag({ itemId: item.id, fromBlock: b.id })}
                    onDragEnd={() => setDrag(null)}
                    onDropBefore={() => {
                      if (drag && drag.itemId !== item.id) moveRow(drag.itemId, b.id, item.id);
                      setDrag(null);
                    }}
                  />
                ))}

                {adding === b.id ? (
                  <AddRow
                    tasks={unplanned}
                    onPick={(taskId) => addRow(b.id, taskId)}
                    onCreate={async (title) => {
                      const bosunId = state.bosuns[0]?.id;
                      if (!bosunId) return;
                      try {
                        const t = await actions.createTask({ bosunId, title });
                        addRow(b.id, t.id);
                      } catch (e) {
                        setError(e instanceof Error ? e.message : String(e));
                      }
                    }}
                    onClose={() => setAdding(null)}
                  />
                ) : (
                  <button type="button" className="today-add" onClick={() => setAdding(b.id)}>
                    <PlusIcon size={11} /> Add to-do
                  </button>
                )}
              </div>
            );
          })
        )}

        {plan !== null && unplanned.length > 0 && (
          <div className="today-tray">
            <button type="button" className="today-tray-head" aria-expanded={trayOpen} onClick={() => setTrayOpen((o) => !o)}>
              <ChevronIcon size={12} open={trayOpen} />
              Not planned · {unplanned.length} open to-do{unplanned.length === 1 ? "" : "s"}
            </button>
            {trayOpen &&
              unplanned.map((t) => (
                <div key={t.id} className="today-tray-row">
                  <button type="button" className="today-tray-title" onClick={() => openRow(t.id)}>
                    <span>{displayTitle(t)}</span>
                    <TaskMeta task={t} />
                  </button>
                  {blocks.length > 0 && (
                    <select
                      aria-label="Add to block"
                      value=""
                      onChange={(e) => {
                        if (e.target.value) addRow(e.target.value, t.id);
                      }}
                    >
                      <option value="">Add to…</option>
                      {blocks.map((b) => (
                        <option key={b.id} value={b.id}>
                          {blockTime(b) ? `${blockTime(b)} · ` : ""}
                          {b.title}
                        </option>
                      ))}
                    </select>
                  )}
                </div>
              ))}
          </div>
        )}
      </div>
    </section>
  );
}

function TaskMeta({ task }: { task: BosunTask }) {
  const ticket = orbitTicketLabel(task);
  if (!task.client && !ticket) return null;
  return (
    <span className="today-meta">
      {task.client && <span>{task.client}</span>}
      {ticket && <span className="today-ticket">{ticket}</span>}
    </span>
  );
}

function PlanRow(props: {
  item: PlanItem;
  task: BosunTask | undefined;
  busy: boolean;
  confirm: Confirm | undefined;
  expanded: boolean;
  blocks: PlanBlock[];
  blockId: string;
  first: boolean;
  last: boolean;
  onToggle: () => void;
  onOpen: () => void;
  onExpand: () => void;
  onEdit: (f: (item: PlanItem) => PlanItem) => void;
  onMove: (toBlock: string) => void;
  onNudge: (delta: number) => void;
  onRemove: () => void;
  onConfirm: (close: boolean) => void;
  onDragStart: () => void;
  onDragEnd: () => void;
  onDropBefore: () => void;
}) {
  const { item, task, busy, confirm, expanded } = props;
  const [stepText, setStepText] = useState("");
  const done = task ? isClosedStatus(task.status) : true;
  const stepsDone = item.steps.filter((s) => s.done).length;

  function addStep() {
    const text = stepText.trim();
    if (!text) return;
    props.onEdit((it) => ({ ...it, steps: [...it.steps, { id: newId(), text, done: false }] }));
    setStepText("");
  }

  return (
    <div
      className={`today-row${done ? " done" : ""}${busy ? " busy" : ""}`}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        props.onDragStart();
      }}
      onDragEnd={props.onDragEnd}
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => {
        e.preventDefault();
        e.stopPropagation();
        props.onDropBefore();
      }}
    >
      <div className="today-row-main">
        <button
          type="button"
          className={`today-check${done ? " on" : ""}`}
          role="checkbox"
          aria-checked={done}
          aria-label={done ? "Mark not done" : "Mark done"}
          disabled={!task || busy}
          onClick={props.onToggle}
        >
          {done && <CheckIcon size={14} />}
        </button>
        <button type="button" className="today-row-body" onClick={task ? props.onOpen : undefined} disabled={!task}>
          <span className="today-row-title">{task ? displayTitle(task) : "This to-do is gone"}</span>
          {task && <TaskMeta task={task} />}
          {task?.nextStep && !done && <span className="today-row-next">{task.nextStep}</span>}
        </button>
        <button
          type="button"
          className={`today-row-more${expanded ? " on" : ""}`}
          aria-label={expanded ? "Hide steps and options" : "Steps and options"}
          aria-expanded={expanded}
          onClick={props.onExpand}
        >
          {item.steps.length > 0 ? (
            <span className="today-steps-count">
              {stepsDone}/{item.steps.length}
            </span>
          ) : (
            <ChevronIcon size={12} open={expanded} />
          )}
        </button>
      </div>

      {confirm && (
        <div className="today-confirm">
          <span>
            Orbit {confirm.ticket} is a {confirm.origin} ticket. Closing it may email them.
          </span>
          <button type="button" className="btn bosun-small-btn" disabled={confirm.busy} onClick={() => props.onConfirm(true)}>
            {confirm.busy ? "Closing…" : "Close in Orbit"}
          </button>
          <button type="button" className="btn bosun-small-btn" disabled={confirm.busy} onClick={() => props.onConfirm(false)}>
            Leave open
          </button>
        </div>
      )}

      {(item.steps.length > 0 || expanded) && (
        <ul className="today-steps">
          {item.steps.map((s) => (
            <li key={s.id} className={s.done ? "done" : ""}>
              <label>
                <input
                  type="checkbox"
                  checked={s.done}
                  onChange={() =>
                    props.onEdit((it) => ({ ...it, steps: it.steps.map((x) => (x.id === s.id ? { ...x, done: !x.done } : x)) }))
                  }
                />
                <span>{s.text}</span>
              </label>
              {expanded && (
                <button
                  type="button"
                  className="icon-btn today-step-x"
                  aria-label="Remove step"
                  onClick={() => props.onEdit((it) => ({ ...it, steps: it.steps.filter((x) => x.id !== s.id) }))}
                >
                  <XIcon size={11} />
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      {expanded && (
        <div className="today-row-options">
          <form
            className="today-step-add"
            onSubmit={(e) => {
              e.preventDefault();
              addStep();
            }}
          >
            <input value={stepText} onChange={(e) => setStepText(e.target.value)} placeholder="Add a step" aria-label="Add a step" />
          </form>
          <div className="today-row-actions">
            <button type="button" className="btn bosun-small-btn" disabled={props.first} onClick={() => props.onNudge(-1)}>
              Up
            </button>
            <button type="button" className="btn bosun-small-btn" disabled={props.last} onClick={() => props.onNudge(1)}>
              Down
            </button>
            {props.blocks.length > 1 && (
              <select aria-label="Move to block" value={props.blockId} onChange={(e) => props.onMove(e.target.value)}>
                {props.blocks.map((b) => (
                  <option key={b.id} value={b.id}>
                    {blockTime(b) ? `${blockTime(b)} · ` : ""}
                    {b.title}
                  </option>
                ))}
              </select>
            )}
            <button type="button" className="btn bosun-small-btn today-remove" onClick={props.onRemove} title="Take it off today's plan; the to-do stays open">
              <TrashIcon size={11} /> Remove
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function BlockEditor({
  block,
  onSave,
  onCancel,
  onDelete,
}: {
  block: PlanBlock;
  onSave: (patch: Partial<PlanBlock>) => void;
  onCancel: () => void;
  onDelete: () => void;
}) {
  const [title, setTitle] = useState(block.title);
  const [start, setStart] = useState(block.start ?? "");
  const [end, setEnd] = useState(block.end ?? "");
  const bad = !!start && !!end && end <= start;
  return (
    <form
      className="today-block-edit"
      onSubmit={(e) => {
        e.preventDefault();
        if (bad) return;
        onSave({ title: title.trim(), start: start || null, end: end || null });
      }}
    >
      <input className="today-block-edit-title" value={title} autoFocus onChange={(e) => setTitle(e.target.value)} aria-label="Block title" />
      <div className="today-block-edit-row">
        <input type="time" value={start} onChange={(e) => setStart(e.target.value)} aria-label="Start" />
        <span>to</span>
        <input type="time" value={end} onChange={(e) => setEnd(e.target.value)} aria-label="End" />
        <button type="submit" className="btn bosun-small-btn" disabled={bad}>
          Save
        </button>
        <button type="button" className="btn bosun-small-btn" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="icon-btn today-block-delete" aria-label="Delete block" title="Delete block" onClick={onDelete}>
          <TrashIcon size={12} />
        </button>
      </div>
      {bad && <span className="today-block-edit-bad">Ends before it starts.</span>}
    </form>
  );
}

function AddRow({
  tasks,
  onPick,
  onCreate,
  onClose,
}: {
  tasks: BosunTask[];
  onPick: (taskId: string) => void;
  onCreate: (title: string) => Promise<void>;
  onClose: () => void;
}) {
  const [q, setQ] = useState("");
  const [busy, setBusy] = useState(false);
  const query = q.trim().toLowerCase();
  const matches = useMemo(() => tasks.filter((t) => taskMatches(t, query)).slice(0, 8), [tasks, query]);
  return (
    <div className="today-addrow">
      <form
        onSubmit={async (e) => {
          e.preventDefault();
          if (!q.trim() || busy) return;
          if (matches.length > 0 && query) {
            onPick(matches[0].id);
          } else {
            setBusy(true);
            await onCreate(q.trim());
            setBusy(false);
          }
          setQ("");
        }}
      >
        <input
          autoFocus
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
          }}
          placeholder="Search open to-dos or name a new one"
          aria-label="Add a to-do to this block"
        />
        <button type="button" className="icon-btn" aria-label="Done adding" onClick={onClose}>
          <XIcon size={13} />
        </button>
      </form>
      <div className="today-addrow-list">
        {matches.map((t, i) => (
          <button
            key={t.id}
            type="button"
            className={`today-addrow-item${i === 0 && query ? " first" : ""}`}
            onClick={() => onPick(t.id)}
          >
            <span>{displayTitle(t)}</span>
            <TaskMeta task={t} />
          </button>
        ))}
        {query && (
          <button type="button" className="today-addrow-item today-addrow-new" disabled={busy} onClick={() => void onCreate(q.trim()).then(() => setQ(""))}>
            <PlusIcon size={11} /> New to-do: "{q.trim()}"
          </button>
        )}
      </div>
    </div>
  );
}
