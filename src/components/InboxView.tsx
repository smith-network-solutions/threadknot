import { useMemo, useState } from "react";
import type { InboxEntry } from "../lib/protocol";
import { ageLabel, TASK_REVIEW_ID } from "../lib/bosun";
import { useStore } from "../state/store";
import { CheckIcon, PanelLeftIcon, XIcon } from "./icons";
import "../styles/inbox.css";

/** `activeTaskId` value that puts the inbox in the main pane (the same slot
 *  the task page and the review use, so every path that closes those closes
 *  this too). */
export const INBOX_ID = "__inbox__";

const FROM_INBOX_KEY = "threadknot.inbox.openedThread";

/** The thread most recently opened from the inbox, so its header can offer
 *  the way back. */
export function threadOpenedFromInbox(): string | null {
  try {
    return sessionStorage.getItem(FROM_INBOX_KEY);
  } catch {
    return null;
  }
}

function rememberOpenedFromInbox(threadId: string | null) {
  try {
    if (threadId) sessionStorage.setItem(FROM_INBOX_KEY, threadId);
    else sessionStorage.removeItem(FROM_INBOX_KEY);
  } catch {
    // ignore
  }
}

/** Notification copy is the agent's markdown; the list shows it as plain
 *  text, so drop the emphasis, code and heading marks. */
function plain(text: string): string {
  return text
    .replace(/\*\*|__|`/g, "")
    .replace(/^#+\s*/gm, "")
    .replace(/\s+/g, " ")
    .trim();
}

const KIND_LABEL: Record<string, string> = {
  turn_completed: "Finished",
  approval_request: "Approval needed",
  question_request: "Question",
  dispatch_finished: "Dispatch",
  bosun: "Bosun",
  error: "Error",
};

/** One row: every notification of one thread (or one thread-less notice),
 *  shown by its newest. */
interface InboxGroup {
  key: string;
  threadId: string;
  latest: InboxEntry;
  entries: InboxEntry[];
  unread: number;
}

function groupEntries(entries: readonly InboxEntry[]): InboxGroup[] {
  const groups = new Map<string, InboxGroup>();
  // Entries arrive newest first, so the first seen is the group's latest.
  for (const e of entries) {
    const key = e.threadId || `notice:${e.id}`;
    let g = groups.get(key);
    if (!g) {
      g = { key, threadId: e.threadId, latest: e, entries: [], unread: 0 };
      groups.set(key, g);
    }
    g.entries.push(e);
    if (!e.readAt) g.unread += 1;
  }
  return [...groups.values()];
}

/** Every notification Threadknot sent (or held back while you were at the
 *  desk), grouped by chat, so a burst of them can be worked through one by
 *  one. Opening a chat reads its notifications on every device. */
export function InboxView() {
  const { state, dispatch, actions } = useStore();
  const [tab, setTab] = useState<"unread" | "all">("unread");
  const inbox = state.inbox;
  const groups = useMemo(() => {
    const all = groupEntries(inbox?.entries ?? []);
    return tab === "unread" ? all.filter((g) => g.unread > 0) : all;
  }, [inbox, tab]);

  function open(g: InboxGroup) {
    if (g.threadId) {
      rememberOpenedFromInbox(g.threadId);
      // selectThread marks the thread's notifications read.
      void actions.selectThread(g.threadId);
    } else {
      // A thread-less notice is a Bosun sweep: its to-dos are on the review.
      void actions.markInboxRead({ ids: g.entries.map((e) => e.id) });
      actions.openTask(TASK_REVIEW_ID);
    }
  }

  const unread = inbox?.unread ?? 0;

  return (
    <section className="thread-pane task-pane inbox-pane">
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
        <div className="thread-head-main task-head-main">
          <span className="task-head-who">Notifications</span>
        </div>
        <button
          type="button"
          className="icon-btn"
          aria-label="Close notifications"
          title="Close"
          onClick={() => actions.openTask(null)}
        >
          <XIcon size={15} />
        </button>
      </header>

      <div className="inbox-tools">
        <div className="inbox-tabs" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={tab === "unread"}
            className={`inbox-tab${tab === "unread" ? " on" : ""}`}
            onClick={() => setTab("unread")}
          >
            Unread {unread > 0 && <span className="inbox-tab-count">{unread}</span>}
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "all"}
            className={`inbox-tab${tab === "all" ? " on" : ""}`}
            onClick={() => setTab("all")}
          >
            All
          </button>
        </div>
        {unread > 0 && (
          <button
            type="button"
            className="btn tone-deny inbox-readall"
            onClick={() => void actions.markInboxRead({ all: true })}
          >
            Mark all read
          </button>
        )}
      </div>

      <div className="inbox-list">
        {inbox === null ? (
          <div className="task-empty">Loading…</div>
        ) : groups.length === 0 ? (
          <div className="task-empty">
            {tab === "unread" ? "All caught up." : "No notifications in the last two weeks."}
          </div>
        ) : (
          groups.map((g) => {
            const e = g.latest;
            const count = tab === "unread" ? g.unread : g.entries.length;
            return (
              <div key={g.key} className={`inbox-row k-${e.kind}${g.unread > 0 ? " unread" : ""}`}>
                <button type="button" className="inbox-main" onClick={() => open(g)}>
                  <span className="inbox-top">
                    <span className="inbox-kind">{KIND_LABEL[e.kind] ?? e.kind}</span>
                    {e.projectName && <span className="inbox-ws">{e.projectName}</span>}
                    {count > 1 && <span className="inbox-count">×{count}</span>}
                    <span className="inbox-when">{ageLabel(e.at)}</span>
                  </span>
                  <span className="inbox-title">{plain(e.threadTitle || e.title)}</span>
                  {e.body && <span className="inbox-body">{plain(e.body)}</span>}
                </button>
                {g.unread > 0 && (
                  <button
                    type="button"
                    className="icon-btn inbox-markread"
                    aria-label={`Mark read: ${e.threadTitle || e.title}`}
                    title="Mark read"
                    onClick={() =>
                      void actions.markInboxRead(
                        g.threadId ? { threadId: g.threadId } : { ids: g.entries.map((x) => x.id) },
                      )
                    }
                  >
                    <CheckIcon size={16} />
                  </button>
                )}
              </div>
            );
          })
        )}
      </div>
    </section>
  );
}

/** "← Notifications" for a chat opened from the inbox. */
export function BackToInbox({ threadId }: { threadId: string }) {
  const { actions } = useStore();
  if (threadOpenedFromInbox() !== threadId) return null;
  return (
    <button
      type="button"
      className="back-to-inbox"
      title="Back to notifications"
      onClick={() => actions.openTask(INBOX_ID)}
    >
      ← Notifications
    </button>
  );
}
