// The Alcove skin's notification toast: one glass card at the top centre of
// the window, carrying the mark of the agent that raised it, what happened
// ("Finished"), a few words about it, and how long ago.
//
// It replaces the corner stack (App.tsx's .toast-stack) only under this skin.
// Same notices, same click target — selecting the thread and dismissing — so
// nothing about when a notice appears changes here.
import { useEffect, useRef, useState } from "react";

import { AgentMark } from "./icons";
import type { Notice } from "../state/store";

/** The heading. Thread events get the Inbox's own wording for their kind; a
 *  locally raised notice ("Voice Parlay", a failed settle) has no kind and
 *  keeps the title it was given. */
const KIND_LABEL: Record<string, string> = {
  turn_completed: "Finished",
  approval_request: "Approval needed",
  question_request: "Question",
  dispatch_finished: "Dispatch",
  bosun: "Bosun",
  error: "Error",
};

function heading(notice: Notice): string {
  return (notice.kind && KIND_LABEL[notice.kind]) || notice.title || "Threadknot";
}

/** Notices live for 8 seconds, so this is "now" nearly always — but a stack
 *  that queued behind a modal should not lie about it. */
function when(at: number | undefined): string {
  if (!at) return "now";
  const m = Math.round((Date.now() - at) / 60000);
  return m < 1 ? "now" : `${m}m`;
}

/** How long the leave animation runs. Keep in step with .completion-toast's
 *  `is-leaving` animation in alcove/overlays.css. */
const LEAVE_MS = 260;

/** The notices to draw: the live ones, plus any that have just been dismissed,
 *  held for the length of their leave animation. Without this a dismissal
 *  unmounts the card mid-air and the exit never plays. */
function useLeaving(notices: Notice[]): Array<{ notice: Notice; leaving: boolean }> {
  const [leaving, setLeaving] = useState<Notice[]>([]);
  const previous = useRef<Notice[]>(notices);
  useEffect(() => {
    const live = new Set(notices.map((n) => n.id));
    const gone = previous.current.filter((n) => !live.has(n.id));
    previous.current = notices;
    if (gone.length === 0) return;
    setLeaving((current) => [...current, ...gone]);
    const timer = setTimeout(() => {
      const ids = new Set(gone.map((n) => n.id));
      setLeaving((current) => current.filter((n) => !ids.has(n.id)));
    }, LEAVE_MS);
    return () => clearTimeout(timer);
  }, [notices]);
  return [
    ...leaving.map((notice) => ({ notice, leaving: true })),
    ...notices.map((notice) => ({ notice, leaving: false })),
  ];
}

export function CompletionToasts({
  notices,
  onOpen,
}: {
  notices: Notice[];
  onOpen: (notice: Notice) => void;
}) {
  const shown = useLeaving(notices);
  if (shown.length === 0) return null;
  return (
    <div className="completion-toast-stack">
      {shown.map(({ notice, leaving }) => (
        <button
          key={notice.id}
          type="button"
          className={`completion-toast glass-surface corner-smoothing${leaving ? " is-leaving" : ""}`}
          onClick={() => onOpen(notice)}
        >
          {notice.agent && (
            <span className="completion-toast__mark">
              <AgentMark agent={notice.agent} size={40} />
            </span>
          )}
          <span className="completion-toast__copy">
            <strong>{heading(notice)}</strong>
            <span>{notice.body}</span>
          </span>
          <span className="completion-toast__when">{when(notice.at)}</span>
        </button>
      ))}
    </div>
  );
}
