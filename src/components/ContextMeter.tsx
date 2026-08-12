import { useEffect, useRef, useState } from "react";
import type { TurnUsage } from "../lib/protocol";
import { formatTokens } from "../lib/format";

/** Percentage of the context window in use, or null if unknown. */
export function usedPercent(u: TurnUsage): number | null {
  if (u.contextPct != null) return u.contextPct;
  if (u.usedTokens != null && u.maxTokens && u.maxTokens > 0) {
    return (u.usedTokens / u.maxTokens) * 100;
  }
  return null;
}

/** Whether a snapshot has enough data to draw a meaningful context ring. */
export function isRenderableUsage(u: TurnUsage | undefined): u is TurnUsage {
  return !!u && usedPercent(u) != null;
}

function fmtPct(pct: number): string {
  return pct < 10 ? `${pct.toFixed(1).replace(/\.0$/, "")}%` : `${Math.round(pct)}%`;
}

/**
 * Circular context-window gauge, ported from t3code's ContextWindowMeter.
 * Ring fills with usage (turns red past 90%); tap/hover reveals used/max.
 */
export function ContextMeter({ usage }: { usage: TurnUsage }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const pct = usedPercent(usage);
  if (pct == null) return null;

  const clamped = Math.max(0, Math.min(100, pct));
  const r = 9.75;
  const circumference = 2 * Math.PI * r;
  const dashOffset = circumference - (clamped / 100) * circumference;
  const over = clamped > 90;
  const used = formatTokens(usage.usedTokens);
  const max = formatTokens(usage.maxTokens);

  return (
    <div
      className="ctx-meter"
      ref={ref}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <button
        type="button"
        className={`ctx-ring${over ? " over" : ""}`}
        aria-label={`Context window ${fmtPct(clamped)} used`}
        onClick={() => setOpen((v) => !v)}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <circle className="ctx-track" cx="12" cy="12" r={r} fill="none" strokeWidth="3" />
          <circle
            className="ctx-fill"
            cx="12"
            cy="12"
            r={r}
            fill="none"
            strokeWidth="3"
            strokeLinecap="round"
            strokeDasharray={circumference}
            strokeDashoffset={dashOffset}
          />
        </svg>
      </button>
      {open && (
        <div className={`ctx-pop${over ? " over" : ""}`} role="dialog">
          <span className="ctx-pop-title">Context</span>
          <span className="ctx-pop-nums">
            {fmtPct(clamped)}
            {used && max && (
              <>
                {" · "}
                {used}/{max}
              </>
            )}
          </span>
        </div>
      )}
    </div>
  );
}
