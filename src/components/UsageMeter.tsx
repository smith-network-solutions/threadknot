import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ProviderUsage, RateWindow } from "../lib/protocol";
import { useStore } from "../state/store";
import { AgentMark } from "./icons";

const AGENT_NAMES: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  kimi: "Kimi Code",
};

function severity(pct: number): "" | "warn" | "hot" {
  if (pct >= 95) return "hot";
  if (pct >= 80) return "warn";
  return "";
}

function fmtPct(pct: number): string {
  return `${Math.round(pct)}%`;
}

/** "resets in 3h 20m" for near windows, weekday + time for far ones. */
function fmtReset(iso?: string): string | null {
  if (!iso) return null;
  const at = new Date(iso).getTime();
  if (Number.isNaN(at)) return null;
  const ms = at - Date.now();
  if (ms <= 0) return "resets now";
  const mins = Math.round(ms / 60000);
  if (mins < 60) return `resets in ${mins}m`;
  if (mins < 48 * 60) return `resets in ${Math.floor(mins / 60)}h ${mins % 60}m`;
  return `resets ${new Date(at).toLocaleString(undefined, {
    weekday: "short",
    hour: "numeric",
    minute: "2-digit",
  })}`;
}

function fmtAge(iso: string): string {
  const mins = Math.round((Date.now() - new Date(iso).getTime()) / 60000);
  if (!Number.isFinite(mins) || mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  return `${Math.floor(mins / 60)}h ago`;
}

function WindowRow({ w }: { w: RateWindow }) {
  const sev = severity(w.usedPercent);
  const reset = fmtReset(w.resetsAt);
  return (
    <div className="usage-row">
      <span className="usage-row-label">{w.label}</span>
      <div className="usage-bar usage-bar-wide">
        <div
          className={`usage-bar-fill ${sev}`}
          style={{ width: `${Math.max(0, Math.min(100, w.usedPercent))}%` }}
        />
      </div>
      <span className={`usage-row-pct ${sev}`}>{fmtPct(w.usedPercent)}</span>
      {reset && <span className="usage-row-reset">{reset}</span>}
    </div>
  );
}

function ProviderBlock({ u }: { u: ProviderUsage }) {
  return (
    <div className="usage-provider">
      <div className="usage-provider-head">
        <span className="usage-provider-name">
          <AgentMark agent={u.agent} size={11} />
          {AGENT_NAMES[u.agent] ?? u.agent}
        </span>
        {u.plan && <span className="usage-plan">{u.plan}</span>}
      </div>
      {u.available && (u.windows ?? []).map((w) => <WindowRow key={w.label} w={w} />)}
      {!u.available && <div className="usage-error">{u.error ?? "unavailable"}</div>}
    </div>
  );
}

/** Compact sidebar summary; click to reveal every provider quota window. */
export function UsageMeter() {
  const { state, actions } = useStore();
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{
    right: number;
    bottom: number;
    width: number;
    maxHeight: number;
  } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);

  // Anchor the portaled popover above the trigger button and keep it there as
  // the window resizes or scrolls. Both the sidebar trigger and the portaled
  // popover render unzoomed (only the message feed zooms), so the trigger's
  // viewport rect maps 1:1 onto the popover's fixed coordinates.
  useLayoutEffect(() => {
    if (!open) return;
    function place() {
      const b = btnRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      setPos({
        right: Math.max(8, window.innerWidth - r.right),
        bottom: Math.max(8, window.innerHeight - r.top + 8),
        width: r.width,
        maxHeight: Math.max(120, r.top - 24),
      });
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (btnRef.current?.contains(t)) return;
      if (popRef.current?.contains(t)) return;
      setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // The startup probe is the source of truth for locally connected providers.
  // Thread focus/status is transient and must never make a subscription meter
  // disappear while that provider remains connected to Threadknot.
  const connectedAgents = new Set(
    (state.hello?.agents ?? [])
      .filter((agent) => agent.available)
      .map((agent) => agent.id),
  );
  const usage = state.usage.filter((u) => connectedAgents.has(u.agent));
  const shown = usage.filter((u) => u.available && (u.windows?.length ?? 0) > 0);
  if (shown.length === 0) return null;

  const newest = usage.reduce((a, b) => (a.fetchedAt > b.fetchedAt ? a : b));

  return (
    <div className="usage-meter">
      <button
        type="button"
        ref={btnRef}
        className="usage-chips"
        aria-label="Provider usage"
        aria-expanded={open}
        title="Subscription usage"
        onClick={() => setOpen((v) => !v)}
      >
        {shown.map((u) => {
          const worst = (u.windows ?? []).reduce((a, b) =>
            a.usedPercent >= b.usedPercent ? a : b,
          );
          const sev = severity(worst.usedPercent);
          return (
            <span key={u.agent} className="usage-chip">
              <AgentMark agent={u.agent} size={12} />
              <span className="usage-bar">
                <span
                  className={`usage-bar-fill ${sev}`}
                  style={{ width: `${Math.max(0, Math.min(100, worst.usedPercent))}%` }}
                />
              </span>
              <span className={`usage-chip-pct ${sev}`}>{fmtPct(worst.usedPercent)}</span>
            </span>
          );
        })}
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            className="usage-pop usage-pop-float"
            role="dialog"
            aria-label="Subscription usage details"
            ref={popRef}
            style={pos}
          >
            {usage.map((u) => (
              <ProviderBlock key={u.agent} u={u} />
            ))}
            <div className="usage-pop-foot">
              <span className="usage-age">updated {fmtAge(newest.fetchedAt)}</span>
              <button
                type="button"
                className="usage-refresh"
                onClick={() => void actions.refreshUsage()}
              >
                refresh
              </button>
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}
