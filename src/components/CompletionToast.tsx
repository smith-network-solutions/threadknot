// The Alcove skin's notification toast: one glass card at the top centre of
// the window, carrying the mark of the agent that raised it, what happened
// ("Finished"), a few words about it, and how long ago.
//
// It replaces the corner stack (App.tsx's .toast-stack) only under this skin.
// Same notices, same click target — selecting the thread and dismissing — so
// nothing about when a notice appears changes here.
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import type { Components } from "react-markdown";
import remarkGfm from "remark-gfm";

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

/** The body, as one line of markdown.
 *
 *  Agents write CommonMark, and the toast was showing it raw — asterisks and
 *  backticks instead of emphasis. The source is flattened to a single line
 *  before parsing rather than after: the card shows one line with an ellipsis,
 *  so a heading or a list in the source would otherwise become a block the
 *  ellipsis cannot act on. Leading block markers go with it, or a reply that
 *  opens with a bullet arrives as a stray dash. */
function oneLine(body: string): string {
  return body
    // Fenced blocks first, as inline code. Collapsed blind, the opening fence
    // would fold into the code and the toast would read "js const x = 1".
    .replace(/```[\w-]*\s*([\s\S]*?)```/g, (_, code: string) => `\`${code.trim().replace(/\s+/g, " ")}\``)
    .replace(/\s*\r?\n\s*/g, " ")
    .replace(/^\s*(?:[-*+]\s+|#{1,6}\s+|>\s+|\d+[.)]\s+)/, "")
    .trim();
}

/** Every block construct rendered as phrasing content.
 *
 *  Two hard requirements, not taste. The card is a <button>, and a <p> or a
 *  <ul> inside one is invalid HTML that the browser hoists back out of it.
 *  And the single-line ellipsis only acts on inline content.
 *
 *  Links become plain text for the same reason: a nested <a> inside a <button>
 *  is invalid and unreachable. Nothing is lost — the card already opens the
 *  thread the text came from, where the link is a real link. */
const INLINE_ONLY: Components = {
  p: ({ children }) => <>{children}</>,
  a: ({ children }) => <>{children}</>,
  code: ({ children }) => <code className="completion-toast__code">{children}</code>,
  pre: ({ children }) => <>{children}</>,
  ul: ({ children }) => <>{children}</>,
  ol: ({ children }) => <>{children}</>,
  li: ({ children }) => <>{children} </>,
  blockquote: ({ children }) => <>{children}</>,
  h1: ({ children }) => <strong>{children}</strong>,
  h2: ({ children }) => <strong>{children}</strong>,
  h3: ({ children }) => <strong>{children}</strong>,
  h4: ({ children }) => <strong>{children}</strong>,
  h5: ({ children }) => <strong>{children}</strong>,
  h6: ({ children }) => <strong>{children}</strong>,
  br: () => <> </>,
  hr: () => null,
  img: () => null,
  table: () => null,
};

/** How long the leave animation runs. Keep in step with .completion-toast's
 *  `is-leaving` animation in alcove/overlays.css. */
const LEAVE_MS = 260;

/** The notices to draw: the live ones, plus any that have just been dismissed,
 *  held for the length of their leave animation. Without this a dismissal
 *  unmounts the card mid-air and the exit never plays.
 *
 *  Each departure owns its own timer, deliberately. A single timer cancelled
 *  by the effect's cleanup was the bug behind cards marching down the screen:
 *  any notice arriving inside the 260ms window cancelled the pending removal,
 *  so the dismissed card never unmounted. It was invisible — the exit
 *  animation ends at opacity 0 — but it still held a row in the stack, and
 *  every card after it started one row lower than the last. */
function useLeaving(notices: Notice[]): Array<{ notice: Notice; leaving: boolean }> {
  const [leaving, setLeaving] = useState<Notice[]>([]);
  const previous = useRef<Notice[]>(notices);
  const timers = useRef(new Map<number, number>());

  useEffect(() => {
    const live = new Set(notices.map((n) => n.id));
    const gone = previous.current.filter((n) => !live.has(n.id));
    previous.current = notices;
    if (gone.length === 0) return;
    setLeaving((current) => [
      ...current.filter((n) => !gone.some((g) => g.id === n.id)),
      ...gone,
    ]);
    for (const notice of gone) {
      window.clearTimeout(timers.current.get(notice.id));
      const timer = window.setTimeout(() => {
        timers.current.delete(notice.id);
        setLeaving((current) => current.filter((n) => n.id !== notice.id));
      }, LEAVE_MS);
      timers.current.set(notice.id, timer);
    }
  }, [notices]);

  // Only on unmount: a pending removal must survive every other update.
  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending.values()) window.clearTimeout(timer);
      pending.clear();
    };
  }, []);

  // Live first, on their way out second: a card leaving can then never push
  // the card that replaces it further down the screen.
  return [
    ...notices.map((notice) => ({ notice, leaving: false })),
    ...leaving.map((notice) => ({ notice, leaving: true })),
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
            <span className="completion-toast__body">
              <ReactMarkdown remarkPlugins={[remarkGfm]} components={INLINE_ONLY} skipHtml>
                {oneLine(notice.body)}
              </ReactMarkdown>
            </span>
          </span>
          <span className="completion-toast__when">{when(notice.at)}</span>
        </button>
      ))}
    </div>
  );
}
