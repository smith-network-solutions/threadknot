import { useState } from "react";
import type { Decision, Wake } from "../lib/protocol";
import { DECISION_LABEL, wakeSummary } from "../lib/bosun";
import { findThread, useStore } from "../state/store";
import { ChevronIcon } from "./icons";

/** A decision kind as a small colored badge. */
export function DecisionBadge({ kind }: { kind: Decision["decision"] }) {
  return <span className={`bosun-badge d-${kind}`}>{DECISION_LABEL[kind] ?? kind}</span>;
}

/** One wake as a collapsible line (`16:02 · 3 signals · ignored 2, …`) that
 *  expands to its decisions. Shared by the sidebar timeline and the panel's
 *  ledger tail. `bosunName` prefixes the line when several bosuns share a
 *  timeline. `onOpen` runs after an "open" link navigated to a thread. */
export function WakeLine({
  wake,
  bosunName,
  onOpen,
}: {
  wake: Wake;
  bosunName?: string;
  onOpen?: () => void;
}) {
  const { state, actions } = useStore();
  const [open, setOpen] = useState(false);
  const failed = !!wake.skipped && wake.decisions.length === 0;
  const hasDetail = wake.decisions.length > 0;
  const wsName = (id?: string | null) =>
    id ? (state.workspaces.find((w) => w.id === id)?.name ?? id) : null;

  return (
    <div className={`bosun-wake${open ? " open" : ""}${failed ? " skipped" : ""}`}>
      <button
        type="button"
        className="bosun-wake-line"
        aria-expanded={hasDetail ? open : undefined}
        disabled={!hasDetail}
        onClick={() => setOpen((v) => !v)}
        title={wake.skipped ?? undefined}
      >
        {hasDetail ? (
          <ChevronIcon size={10} open={open} className="row-chevron" />
        ) : (
          <span className="bosun-wake-spacer" />
        )}
        <span className="bosun-wake-text">
          {bosunName && <span className="bosun-wake-who">{bosunName} · </span>}
          {wakeSummary(wake)}
        </span>
      </button>
      {open && hasDetail && (
        <ul className="bosun-decisions">
          {wake.decisions.map((d) => {
            const ws = wsName(d.workspaceId);
            const canOpen = !!d.threadId && !!findThread(state, d.threadId);
            return (
              <li key={d.signalId} className="bosun-decision">
                <div className="bosun-decision-head">
                  <DecisionBadge kind={d.decision} />
                  <span className="bosun-decision-title" title={d.title}>
                    {d.title || d.signalId}
                  </span>
                  {d.threadId && (
                    <button
                      type="button"
                      className="sched-view"
                      disabled={!canOpen}
                      title={canOpen ? "Open the thread" : "Thread not loaded here"}
                      onClick={() => {
                        void actions.selectThread(d.threadId!);
                        onOpen?.();
                      }}
                    >
                      open
                    </button>
                  )}
                </div>
                <div className="bosun-decision-meta">
                  {ws && <span className="sched-project">{ws}</span>}
                  <span className="bosun-decision-reason">{d.reason}</span>
                </div>
                {d.error && <div className="sched-error">{d.error}</div>}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
