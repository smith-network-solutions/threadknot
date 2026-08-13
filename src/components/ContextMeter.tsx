import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type RefObject,
} from "react";
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
export function ContextMeter({
  usage,
  anchorRef,
  onCompact,
}: {
  usage: TurnUsage;
  /** Optional element to center the popover over instead of the meter itself. */
  anchorRef?: RefObject<HTMLElement | null>;
  /** Run the agent's /compact command from the tooltip action. */
  onCompact: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [pinned, setPinned] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const [popStyle, setPopStyle] = useState<CSSProperties>();

  const positionPopover = useCallback(() => {
    const meter = ref.current;
    const pop = popRef.current;
    const anchor = anchorRef?.current ?? meter?.closest<HTMLElement>(".composer-card");
    const container = meter?.closest<HTMLElement>(".composer-card") ?? anchor;
    if (!meter || !pop || !anchor || !container) return;

    const anchorRect = anchor.getBoundingClientRect();
    const containerRect = container.getBoundingClientRect();
    const popRect = pop.getBoundingClientRect();
    const edge = 12;
    const centeredLeft =
      anchorRect.left - containerRect.left + anchorRect.width / 2 - popRect.width / 2;
    const left = Math.max(edge, Math.min(centeredLeft, containerRect.width - popRect.width - edge));
    const top = anchorRect.top - containerRect.top - popRect.height - 12;

    setPopStyle({
      position: "absolute",
      left,
      top: top - 20,
      bottom: "auto",
      transform: "none",
    });
  }, [anchorRef]);

  useLayoutEffect(() => {
    if (!open) {
      setPopStyle(undefined);
      return;
    }

    const update = () => requestAnimationFrame(positionPopover);
    update();
    window.addEventListener("resize", update);
    const anchor = anchorRef?.current;
    const observer = anchor && typeof ResizeObserver !== "undefined"
      ? new ResizeObserver(update)
      : null;
    if (observer && anchor) observer.observe(anchor);
    return () => {
      window.removeEventListener("resize", update);
      observer?.disconnect();
    };
  }, [anchorRef, open, positionPopover]);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
        setPinned(false);
      }
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
      onMouseLeave={() => {
        if (!pinned) setOpen(false);
      }}
    >
      <button
        type="button"
        className={`ctx-ring${over ? " over" : ""}`}
        aria-label={`Context window ${fmtPct(clamped)} used`}
        aria-expanded={open}
        aria-pressed={pinned}
        onClick={() => {
          const nextPinned = !pinned;
          setPinned(nextPinned);
          setOpen(nextPinned);
        }}
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
        <div
          ref={popRef}
          className={`ctx-pop${over ? " over" : ""}`}
          role="dialog"
          style={popStyle}
        >
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
          <button
            type="button"
            className="ctx-compact-btn"
            onClick={onCompact}
            title="Run /compact"
          >
            Compact
          </button>
        </div>
      )}
    </div>
  );
}
