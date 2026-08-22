import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { SubagentInfo } from "../state/feed";
import { formatDuration } from "../lib/format";
import { useIsMobile } from "../lib/viewport";
import { useSheetClose, useSheetDrag } from "./Sheet";
import { ChevronIcon, XIcon } from "./icons";

/** Order: running first (oldest-launched first within each group). */
function ordered(subs: SubagentInfo[]): SubagentInfo[] {
  const rank = (s: SubagentInfo) => (s.status === "running" ? 0 : s.status === "error" ? 1 : 2);
  return subs.map((s, i) => ({ s, i })).sort((a, b) => rank(a.s) - rank(b.s) || a.i - b.i).map((x) => x.s);
}

function statusGlyph(status: SubagentInfo["status"]) {
  if (status === "running") return <span className="tool-spin" aria-label="running" />;
  if (status === "error") return <span className="agent-hud-x" aria-label="error">×</span>;
  return <span className="agent-hud-check" aria-label="done">✓</span>;
}

/** The most useful one-liner for a subagent: its result when done, else its
 *  latest streamed activity (synchronous agents), else its prompt/description. */
export function elapsed(s: SubagentInfo, now: number): string | null {
  if (!s.startedAt) return null;
  const started = Date.parse(s.startedAt);
  return Number.isFinite(started) ? formatDuration(Math.max(0, now - started)) : null;
}

function activityAge(s: SubagentInfo, now: number): string | null {
  if (!s.lastActivityAt) return null;
  const updated = Date.parse(s.lastActivityAt);
  return Number.isFinite(updated) ? formatDuration(Math.max(0, now - updated)) : null;
}

function detailLine(s: SubagentInfo, now: number): string {
  if (s.summary) return s.summary;
  const last = s.activity[s.activity.length - 1];
  const age = elapsed(s, now);
  const updated = activityAge(s, now);
  if (last) {
    return `${last.text}${age ? ` · ${age} elapsed` : ""}${updated ? ` · updated ${updated} ago` : ""}`;
  }
  return `working${age ? ` · ${age}` : "…"}`;
}

const AGENT_LABEL: Record<string, string> = {
  claude: "Claude",
  codex: "Codex",
  kimi: "Kimi",
  hermes: "Hermes",
  claudex: "Claudex",
};

export function agentLabel(agent: string): string {
  return AGENT_LABEL[agent] ?? agent;
}

/**
 * A dispatched worker has a real thread of its own. Keeping it one click away
 * is the whole reason a dispatch is a thread and not a summary.
 *
 * Shared with the feed's subagent card: the HUD unmounts the moment the last
 * agent finishes, so if this button lived only there, finishing the work would
 * delete the only route to it.
 */
export function OpenWorkerButton({
  childThreadId,
  onOpenThread,
  className = "agent-hud-open-thread",
}: {
  childThreadId?: string;
  onOpenThread?: (threadId: string) => void;
  className?: string;
}) {
  if (!childThreadId || !onOpenThread) return null;
  return (
    <button type="button" className={className} onClick={() => onOpenThread(childThreadId)}>
      open its thread →
    </button>
  );
}

function AgentRow({
  s,
  now,
  onOpenThread,
  defaultOpen = false,
}: {
  s: SubagentInfo;
  now: number;
  onOpenThread?: (threadId: string) => void;
  /** Expanded on mount. Set for the lone running agent, where a list of one
   *  collapsed row answers nothing the status line hadn't already said. */
  defaultOpen?: boolean;
}) {
  const expandable = s.status === "running" || s.activity.length > 0 || !!s.summary || !!s.prompt;
  const [open, setOpen] = useState(defaultOpen && expandable);
  const d = s.dispatch;
  return (
    <div className={`agent-hud-row status-${s.status}${open ? " open" : ""}`}>
      <button
        type="button"
        className="agent-hud-row-head-btn"
        onClick={() => expandable && setOpen((v) => !v)}
        disabled={!expandable}
        aria-expanded={open}
      >
        <span className="agent-hud-row-glyph">{statusGlyph(s.status)}</span>
        <div className="agent-hud-row-body">
          <div className="agent-hud-row-head">
            <span className="agent-hud-row-name">{s.description || "subagent"}</span>
            {d ? (
              <>
                <span className="agent-hud-badge is-agent">{agentLabel(d.agent)}</span>
                <span className="agent-hud-badge is-machine" title={d.machineId}>
                  {d.machineName}
                </span>
              </>
            ) : (
              <span className="agent-hud-badge">
                {s.background ? "bg" : s.subagentType || "agent"}
              </span>
            )}
          </div>
          {!open && <div className="agent-hud-row-detail">{detailLine(s, now)}</div>}
        </div>
        {expandable && <ChevronIcon size={13} open={open} className="agent-hud-row-chevron" />}
      </button>
      <OpenWorkerButton childThreadId={d?.childThreadId} onOpenThread={onOpenThread} />
      {open && (
        <div className="agent-hud-activity">
          {s.prompt && (
            <div className="agent-hud-act-line kind-prompt">
              <span className="agent-hud-act-tag">brief</span>
              <span className="agent-hud-act-text">{s.prompt}</span>
            </div>
          )}
          {s.summary && (
            <div className="agent-hud-act-line is-result">
              <span className="agent-hud-act-tag">result</span>
              <span className="agent-hud-act-text">{s.summary}</span>
            </div>
          )}
          {s.activity.map((a, i) => (
            <div key={i} className={`agent-hud-act-line kind-${a.activity}`}>
              <span className="agent-hud-act-tag">{a.activity}</span>
              <span className="agent-hud-act-text">{a.text}</span>
            </div>
          ))}
          {!s.summary && s.activity.length === 0 && (
            <div className="agent-hud-act-empty">
              Waiting for child activity · {elapsed(s, now) ?? "just started"}.
              The call is still open; use Stop if it is taking unreasonably long.
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The panel body, identical in the desktop popover and the phone sheet. */
function AgentList({
  subagents,
  now,
  onOpenThread,
}: {
  subagents: SubagentInfo[];
  now: number;
  onOpenThread?: (threadId: string) => void;
}) {
  const rows = ordered(subagents);
  const running = rows.filter((s) => s.status === "running").length;
  return (
    <div className="agent-hud-pop-list">
      {rows.map((s) => (
        <AgentRow
          key={s.taskId}
          s={s}
          now={now}
          onOpenThread={onOpenThread}
          defaultOpen={running === 1 && s.status === "running"}
        />
      ))}
    </div>
  );
}

function headLine(running: number, done: number): string {
  return `${running} running${done > 0 ? ` · ${done} done` : ""}`;
}

/**
 * Phone presentation: the same list as a bottom sheet, reusing the Settings
 * sheet's grip, drag gesture and exit timing rather than inventing a second
 * dismissal idiom. The anchored popover is a mouse affordance — pinned to an
 * 11px chip in the corner, it is not something you find with a thumb.
 */
function AgentSheet({
  subagents,
  now,
  running,
  done,
  onOpenThread,
  onClose,
}: {
  subagents: SubagentInfo[];
  now: number;
  running: number;
  done: number;
  onOpenThread?: (threadId: string) => void;
  onClose: () => void;
}) {
  const { surfaceRef, closing, requestClose } = useSheetClose(onClose);
  const { dragging, dragHandlers } = useSheetDrag(surfaceRef, requestClose);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") requestClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [requestClose]);

  return createPortal(
    <div
      className={`agent-sheet-backdrop${closing ? " closing" : ""}`}
      onClick={requestClose}
    >
      <div
        ref={surfaceRef}
        className={`agent-sheet${closing ? " closing" : ""}${dragging ? " dragging" : ""}`}
        role="dialog"
        aria-label="Agent activity"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="ss-sheet-grab" {...dragHandlers}>
          <div className="ss-grip" aria-hidden="true" />
          <header className="agent-sheet-head">
            <span className="agent-sheet-title">
              {running === 1 ? "1 agent working" : `${running} agents working`}
            </span>
            {done > 0 && <span className="agent-hud-done">+{done} done</span>}
            <button
              type="button"
              className="icon-btn agent-sheet-close"
              aria-label="Close agent activity"
              onClick={requestClose}
            >
              <XIcon size={16} />
            </button>
          </header>
        </div>
        <AgentList subagents={subagents} now={now} onOpenThread={onOpenThread} />
      </div>
    </div>,
    document.body,
  );
}

/**
 * Indicator of the turn's subagents: a count pill above the composer, and the
 * panel behind it listing what each one is doing.
 *
 * The panel is controlled by ThreadView because the working status line opens
 * it too — that line is the thing a reader actually looks at while a thread is
 * busy, so it, not this chip, is the primary way in.
 */
export function AgentHud({
  subagents,
  onOpenThread,
  open,
  onOpenChange,
}: {
  subagents: SubagentInfo[];
  /** Open a dispatched worker's own thread. Absent in contexts that cannot
   *  navigate (the mobile feed), where the row simply is not clickable. */
  onOpenThread?: (threadId: string) => void;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [pos, setPos] = useState<{ right: number; bottom: number; maxHeight: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const isMobile = useIsMobile();

  const running = subagents.filter((s) => s.status === "running").length;
  const done = subagents.length - running;
  const [now, setNow] = useState(Date.now());

  useEffect(() => {
    if (running === 0) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [running]);

  // Close when the work finishes so a stale panel doesn't linger.
  useEffect(() => {
    if (running === 0) onOpenChange(false);
  }, [running, onOpenChange]);

  const anchored = open && !isMobile;

  useLayoutEffect(() => {
    if (!anchored) return;
    function place() {
      const b = btnRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      setPos({
        right: Math.max(8, window.innerWidth - r.right),
        bottom: Math.max(8, window.innerHeight - r.top + 8),
        maxHeight: Math.max(160, r.top - 24),
      });
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [anchored]);

  useEffect(() => {
    if (!anchored) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (btnRef.current?.contains(t) || popRef.current?.contains(t)) return;
      onOpenChange(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onOpenChange(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [anchored, onOpenChange]);

  if (running === 0) return null;

  return (
    <div className="agent-hud-dock">
      <button
        type="button"
        ref={btnRef}
        className="agent-hud-pill"
        aria-expanded={open}
        aria-label={`${running} child agent${running === 1 ? "" : "s"} running`}
        onClick={() => onOpenChange(!open)}
      >
        <span className="tool-spin" />
        <span className="agent-hud-count">{running}</span>
        <span className="agent-hud-word">agent{running === 1 ? "" : "s"}</span>
        {running === 1 && (
          <span className="agent-hud-done">{elapsed(subagents.find((s) => s.status === "running")!, now)}</span>
        )}
        {done > 0 && <span className="agent-hud-done">+{done} done</span>}
      </button>
      {open && isMobile && (
        <AgentSheet
          subagents={subagents}
          now={now}
          running={running}
          done={done}
          onOpenThread={onOpenThread}
          onClose={() => onOpenChange(false)}
        />
      )}
      {anchored &&
        pos &&
        createPortal(
          <div
            className="agent-hud-pop"
            role="dialog"
            aria-label="Subagent activity"
            ref={popRef}
            style={{ right: pos.right, bottom: pos.bottom, maxHeight: pos.maxHeight }}
          >
            <div className="agent-hud-pop-head">{headLine(running, done)}</div>
            <AgentList subagents={subagents} now={now} onOpenThread={onOpenThread} />
          </div>,
          document.body,
        )}
    </div>
  );
}
