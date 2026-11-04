import { useMemo, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { Notification03Icon } from "@hugeicons/core-free-icons";
import type { InboxEntry } from "../lib/protocol";
import { ageLabel, TASK_REVIEW_ID } from "../lib/bosun";
import { useStore } from "../state/store";
import { Dropdown } from "./alcove/Dropdown";
import { Tooltip } from "./alcove/Tooltip";
import { CheckIcon, PanelLeftIcon, XIcon } from "./icons";
import "../styles/inbox.css";

/** `activeTaskId` value that puts the inbox in the main pane (the same slot
 *  the task page and the review use, so every path that closes those closes
 *  this too). */
export const INBOX_ID = "__inbox__";

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

/** The Alcove skin's notifications control: the bell in the sidebar's top-right
 *  corner, and the glass popover it opens — the same material and motion as the
 *  model picker and the usage meter, so the three corners of the app that drop
 *  a panel all drop the same one.
 *
 *  Deliberately a summary, not a second inbox: the newest groups, read or open
 *  in one click. The page (`InboxView`) is still there, reachable the way it
 *  always was, and stays the place that can filter and work a backlog. */
export function InboxMenu() {
  const { state, actions } = useStore();
  const [menuOpen, setMenuOpen] = useState(false);
  const inbox = state.inbox;
  const unread = inbox?.unread ?? 0;
  // Enough to cover a burst without turning the popover into a scroll pit; the
  // page is one row away for anything past this.
  const groups = useMemo(() => groupEntries(inbox?.entries ?? []).slice(0, 8), [inbox]);

  function open(g: InboxGroup) {
    if (g.threadId) {
      void actions.selectThread(g.threadId);
    } else {
      void actions.markInboxRead({ ids: g.entries.map((e) => e.id) });
      actions.openTask(TASK_REVIEW_ID);
    }
  }

  return (
    <Dropdown
      className="inbox-menu-root"
      menuClassName="inbox-menu"
      placement="bottom-right"
      offset={2}
      onOpenChange={setMenuOpen}
      trigger={
        <Tooltip content="Notifications" placement="bottom" disabled={menuOpen}>
          <button
            type="button"
            className={`icon-btn sidebar-inbox-btn${unread > 0 ? " has-unread" : ""}`}
            aria-label={unread > 0 ? `Notifications — ${unread} unread` : "Notifications"}
            aria-haspopup="menu"
          >
            <HugeiconsIcon icon={Notification03Icon} size={26} strokeWidth={1.7} />
            {unread > 0 && (
              <span className="inbox-btn-count">{unread > 99 ? "99+" : unread}</span>
            )}
          </button>
        </Tooltip>
      }
    >
      <div className="inbox-menu__head">
        <span className="inbox-menu__title">Notifications</span>
        {unread > 0 && (
          <button
            type="button"
            className="inbox-menu__readall"
            // The menu closes on any click inside it (see Dropdown). Reading
            // everything is a thing you do and then keep looking, so this one
            // keeps the panel open.
            onClick={(e) => {
              e.stopPropagation();
              void actions.markInboxRead({ all: true });
            }}
          >
            Mark all read
          </button>
        )}
      </div>
      <div className="inbox-menu__list">
        {inbox === null ? (
          <div className="inbox-menu__empty">Loading…</div>
        ) : groups.length === 0 ? (
          <div className="inbox-menu__empty">All caught up.</div>
        ) : (
          groups.map((g) => {
            const e = g.latest;
            return (
              <button
                key={g.key}
                type="button"
                className={`inbox-menu__row${g.unread > 0 ? " unread" : ""}`}
                onClick={() => open(g)}
              >
                <span className="inbox-menu__top">
                  <span className="inbox-menu__kind">{KIND_LABEL[e.kind] ?? e.kind}</span>
                  <span className="inbox-menu__when">{ageLabel(e.at)}</span>
                </span>
                <span className="inbox-menu__name">{plain(e.threadTitle || e.title)}</span>
                {e.body && <span className="inbox-menu__body">{plain(e.body)}</span>}
              </button>
            );
          })
        )}
      </div>
    </Dropdown>
  );
}
