import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  HERMES_HOME_PROJECT_ID,
  isQuickHomeProjectId,
  threadParticipants,
} from "../lib/protocol";
import {
  formatCompactDateTime,
  formatDuration,
  formatFullDateTime,
} from "../lib/format";
import {
  APPEARANCE_EVENT,
  ZOOM_APPLIED_EVENT,
  getAppearance,
  getAppliedZoom,
  setAppearance,
} from "../lib/appearance";
import { findThread, resolveProjectView, useStore } from "../state/store";
import { useAvatarHoverPreview } from "./AvatarHoverPreview";
import { FeedItemView } from "./FeedItems";
import { LaneChips, ReviewMenu } from "./ReviewMenu";
import { AgentHud } from "./AgentHud";
import { activeSubagents } from "../state/feed";
import { Composer } from "./Composer";
import { AgentMark, ArrowDownIcon, MenuIcon, PencilIcon } from "./icons";
import { VISIBLE_TABS } from "./WorkspacePanel";

/** How close to the end we can be before showing the jump-to-present button. */
const BOTTOM_SLACK = 90;
/** Auto-follow only engages at the real end; a larger threshold makes manual
 * scrolling snap the final stretch and keeps tugging the reader back down. */
const BOTTOM_STICK_EPSILON = 2;

/** Persistent chip showing the applied conversation zoom ("115%"). Sits in the
 *  thread header, which stays at 1x along with the composer: only the message
 *  feed below scales, so the chip is a fixed-size readout of what the log is
 *  doing. Click resets the zoom to 100%. */
function ZoomChip() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const bump = () => setTick((n) => n + 1);
    window.addEventListener(APPEARANCE_EVENT, bump);
    // Feed-only zoom has no dynamic cap so this never fires today; kept wired
    // so the readout stays correct if an applied-vs-stored split returns.
    window.addEventListener(ZOOM_APPLIED_EVENT, bump);
    return () => {
      window.removeEventListener(APPEARANCE_EVENT, bump);
      window.removeEventListener(ZOOM_APPLIED_EVENT, bump);
    };
  }, []);
  const applied = getAppliedZoom();
  return (
    <button
      type="button"
      className="zoom-chip"
      title="conversation zoom: click to reset to 100%"
      aria-label={`Conversation zoom ${Math.round(applied * 100)}%; click to reset to 100%`}
      onClick={() => {
        const a = getAppearance();
        if (a.uiZoom !== 1) setAppearance({ ...a, uiZoom: 1 });
      }}
    >
      {Math.round(applied * 100)}%
    </button>
  );
}

function ThreadTiming({
  createdAt,
  feed,
}: {
  createdAt: string;
  feed: ReturnType<typeof useStore>["state"]["feed"];
}) {
  let lastMessageAt: string | undefined;
  for (let i = feed.length - 1; i >= 0; i--) {
    const item = feed[i];
    if ((item.type === "user" || item.type === "assistant") && item.timestamp) {
      lastMessageAt = item.timestamp;
      break;
    }
  }
  const startedMs = Date.parse(createdAt);
  const lastMs = lastMessageAt ? Date.parse(lastMessageAt) : Number.NaN;
  const elapsedMs =
    Number.isFinite(startedMs) && Number.isFinite(lastMs)
      ? Math.max(0, lastMs - startedMs)
      : null;
  const startedTitle = formatFullDateTime(createdAt);
  const lastTitle = lastMessageAt ? formatFullDateTime(lastMessageAt) : "";

  return (
    <span className="thread-timing" aria-label="Thread timing">
      <span className="thread-timing-item" title={startedTitle}>
        <b>Started</b>
        <time dateTime={createdAt}>{formatCompactDateTime(createdAt)}</time>
      </span>
      {lastMessageAt && (
        <>
          <span className="thread-timing-dot" aria-hidden="true">·</span>
          <span className="thread-timing-item" title={lastTitle}>
            <b>Last</b>
            <time dateTime={lastMessageAt}>{formatCompactDateTime(lastMessageAt)}</time>
          </span>
        </>
      )}
      {elapsedMs != null && (
        <>
          <span className="thread-timing-dot" aria-hidden="true">·</span>
          <span
            className="thread-timing-item"
            title={`Thread span: ${startedTitle} to ${lastTitle}`}
          >
            <b>Elapsed</b>
            <span>{formatDuration(elapsedMs)}</span>
          </span>
        </>
      )}
    </span>
  );
}

export function ThreadView() {
  const { state, dispatch, actions } = useStore();
  const thread = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
  const draft = state.draft;
  const project = resolveProjectView(
    state,
    thread ? thread.projectId : draft?.projectId,
  )?.project;
  const agentInfo = state.hello?.agents.find(
    (a) => a.id === (thread ? thread.agent : draft?.agent),
  );
  // Hermes home threads have no folder: no path chip, no Files/Git/Terminal.
  const hermesHome =
    (thread ? thread.projectId : draft?.projectId) === HERMES_HOME_PROJECT_ID;
  const quickHome = isQuickHomeProjectId(
    thread ? thread.projectId : draft?.projectId,
  );
  const quickAccess = (thread ? thread.settings : draft?.settings)?.access;
  const hermesGatewayName = hermesHome
    ? state.hello?.agents
        .find((a) => a.id === "hermes")
        ?.models.find((m) => m.id === (thread ? thread.settings : draft?.settings)?.model)
        ?.name
    : undefined;
  // The gateway's profile photo replaces the brand mark in the header chip.
  const hermesRec = hermesHome
    ? state.hermesAgents.find(
        (a) => a.id === (thread ? thread.settings : draft?.settings)?.model,
      )
    : undefined;
  const hermesAvatar = hermesRec?.avatar ?? hermesRec?.image;
  const chipPreview = useAvatarHoverPreview({
    image: hermesAvatar,
    name: hermesGatewayName ?? hermesRec?.name ?? "hermes agent",
  });

  const [editing, setEditing] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");

  const scrollRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  const scrollTopRef = useRef(0);
  // The zoom scrollTopRef's number was measured under. Compared (not just used)
  // so one zoom value is only ever rescaled once (see the APPEARANCE_EVENT
  // handler below).
  const zoomRef = useRef(getAppliedZoom());
  const scrollFeedRef = useRef<string | null>(null);
  const loadedFeedId =
    state.activeThreadId && state.feedThreadId === state.activeThreadId
      ? state.activeThreadId
      : null;
  const observerRef = useRef<ResizeObserver | null>(null);
  const [atPresent, setAtPresent] = useState(true);
  const feedLen = state.feed.length;
  const subagents = useMemo(() => activeSubagents(state.feed), [state.feed]);
  const runningSubagents = subagents.filter((subagent) => subagent.status === "running").length;

  // Scroll bookkeeping is all done on .feed-scroll, which is NOT zoomed (the
  // conversation zoom sits on .feed-inner inside it), so scrollTop,
  // scrollHeight and clientHeight are all true screen px and stay comparable:
  // a zoom change simply grows or shrinks scrollHeight the way new messages
  // would, and both the pin-to-bottom writes and the BOTTOM_SLACK test below
  // keep working without a single zoom term.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!loadedFeedId || !el) return;

    if (scrollFeedRef.current !== loadedFeedId) {
      scrollFeedRef.current = loadedFeedId;
      stickRef.current = true;
    }

    if (stickRef.current) {
      el.scrollTop = el.scrollHeight;
      scrollTopRef.current = el.scrollTop;
    } else if (el.scrollTop !== scrollTopRef.current) {
      // WebKit can reset a nested scroller when unrelated app state rerenders.
      // Keep the reader's last explicit position unless they were following the end.
      el.scrollTop = scrollTopRef.current;
    }
    setAtPresent((prev) => (prev === stickRef.current ? prev : stickRef.current));
  });

  const jumpToPresent = useCallback(() => {
    const el = scrollRef.current;
    stickRef.current = true;
    setAtPresent(true);
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    scrollTopRef.current = el.scrollTop;
  }, []);

  // The effect above only fires on a React render. Feed content also settles
  // outside of one: images and videos resolve, fonts swap, code blocks reflow,
  // and the zoom multiplier changes the rendered height of everything at once.
  // Without this, following the end drifts backwards as the log grows under it.
  // The callback never reads the observed size, only re-pins the scroller, so
  // the observer's own (zoomed-local) measurements are irrelevant.
  const feedInnerRef = useCallback((node: HTMLDivElement | null) => {
    observerRef.current?.disconnect();
    observerRef.current = null;
    if (!node) return;
    const ro = new ResizeObserver(() => {
      const el = scrollRef.current;
      if (!el || !stickRef.current) return;
      el.scrollTop = el.scrollHeight;
      scrollTopRef.current = el.scrollTop;
    });
    ro.observe(node);
    observerRef.current = ro;
  }, []);

  useEffect(() => () => observerRef.current?.disconnect(), []);

  // Zoom moves the reader unless we move them back. scrollTop is screen px and
  // the zoom lives on .feed-inner INSIDE the scrollport, so the content
  // coordinate under any given scrollTop scales by z2/z1 when the zoom changes:
  // reusing the old number lands somewhere else in the log entirely. Two cases:
  // following the end means re-pin to the new end, anything else means rescale
  // the position so the CENTRE of the viewport keeps showing the same text
  // (top-anchoring drifts the line being read off the top at high zoom).
  //
  // This cooperates with the ResizeObserver above rather than racing it: both
  // can fire for a single zoom change, so both operations are safe to repeat.
  // Re-pinning is idempotent by construction, and the rescale is made so by
  // zoomRef, which is advanced before the write and gates the whole handler.
  // The observer only ever re-pins (and only while sticking), so it can never
  // rescale a second time; APPEARANCE_EVENT covers the pure-zoom repaint the
  // observer may not report at all.
  useEffect(() => {
    function onAppearance() {
      const from = zoomRef.current;
      const to = getAppliedZoom();
      // Theme, accent and font edits all broadcast on this event too.
      if (to === from) return;
      zoomRef.current = to;
      const el = scrollRef.current;
      if (!el) return;
      if (stickRef.current) {
        el.scrollTop = el.scrollHeight;
        scrollTopRef.current = el.scrollTop;
        return;
      }
      // The style is already applied (setAppearance writes --ui-zoom before it
      // broadcasts), so the scroller may have clamped scrollTop under us
      // already on a zoom out; scrollTopRef still holds the pre-zoom number.
      // clientHeight is the unzoomed scrollport and does not change here.
      const view = el.clientHeight;
      el.scrollTop = Math.max(0, (scrollTopRef.current + view / 2) * (to / from) - view / 2);
      scrollTopRef.current = el.scrollTop;
    }
    window.addEventListener(APPEARANCE_EVENT, onAppearance);
    return () => window.removeEventListener(APPEARANCE_EVENT, onAppearance);
  }, []);

  const busy = thread && thread.status !== "idle";
  // Multi-lane threads get a two-row header on phones (the lane roster drops
  // to its own scrolling strip) — see the mobile `.thread-head.has-lanes` CSS.
  const hasLanes = !!thread && threadParticipants(thread).length > 1;

  if (!thread && !draft) return <EmptyPane />;

  return (
    <section className="thread-pane">
      <header className={`thread-head${hasLanes ? " has-lanes" : ""}`}>
        <button
          className="icon-btn hamburger"
          aria-label="Open sidebar"
          onClick={() => dispatch({ type: "sidebar", open: true })}
        >
          <MenuIcon size={18} />
        </button>
        <div className="thread-head-main">
          {thread && editing ? (
            <input
              className="title-input"
              autoFocus
              value={titleDraft}
              onChange={(e) => setTitleDraft(e.target.value)}
              onBlur={() => {
                setEditing(false);
                const t = titleDraft.trim();
                if (t && t !== thread.title) void actions.renameThread(thread.id, t);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                if (e.key === "Escape") setEditing(false);
              }}
            />
          ) : (
            <h1 className="thread-title">
              {thread
                ? thread.title || "Untitled thread"
                : quickHome
                  ? "New quick thread"
                  : "New thread"}
              {thread && (
                <button
                  className="icon-btn title-edit"
                  aria-label="Rename thread"
                  onClick={() => {
                    setTitleDraft(thread.title);
                    setEditing(true);
                  }}
                >
                  <PencilIcon size={13} />
                </button>
              )}
            </h1>
          )}
          <div className="thread-sub">
            {agentInfo && (
              <span className="agent-chip">
                {hermesAvatar ? (
                  <span className="chip-avatar" {...chipPreview.hoverProps}>
                    <img src={hermesAvatar} alt="" />
                  </span>
                ) : (
                  <AgentMark agent={agentInfo.id} size={11} />
                )}
                {hermesGatewayName ?? agentInfo.name}
              </span>
            )}
            {chipPreview.portal}
            {thread && <LaneChips thread={thread} />}
            {quickHome && (
              <span
                className={`thread-scope-chip${quickAccess === "full" ? " broad" : ""}`}
                title={
                  quickAccess === "full"
                    ? "Full access is enabled for this computer"
                    : "Starts in an isolated scratch directory"
                }
              >
                {quickAccess === "full" ? "computer access" : "scratch cwd"}
              </span>
            )}
            {project && !hermesHome && !quickHome && (
              <span className="thread-path">{project.path}</span>
            )}
            {thread && <ThreadTiming createdAt={thread.createdAt} feed={state.feed} />}
          </div>
        </div>
        {busy && (
          <div className="head-status">
            <span className="sonar" />
            {thread!.parley
              ? `parley · round ${thread!.parley.round}/${thread!.parley.maxRounds}`
              : thread!.status === "waiting_approval"
                ? "awaiting approval"
                : "underway"}
          </div>
        )}
        <div className="ws-toggles">
          {thread && <ReviewMenu thread={thread} />}
          {!hermesHome && !quickHome && VISIBLE_TABS.map(({ id, label, Icon }) => {
            const openTab = project ? state.workspace[project.id] ?? null : null;
            return (
              <button
                key={id}
                type="button"
                className={`icon-btn${openTab === id ? " on" : ""}`}
                aria-label={label}
                title={label}
                disabled={!project}
                onClick={() =>
                  project &&
                  dispatch({
                    type: "workspace",
                    projectId: project.id,
                    tab: openTab === id ? null : id,
                  })
                }
              >
                <Icon size={16} />
              </button>
            );
          })}
        </div>
        <ZoomChip />
      </header>

      <div
        className="feed-scroll"
        ref={scrollRef}
        onScroll={(e) => {
          if (!loadedFeedId) return;
          const el = e.currentTarget;
          scrollTopRef.current = el.scrollTop;
          const distanceFromEnd = Math.max(
            0,
            el.scrollHeight - el.scrollTop - el.clientHeight,
          );
          // Being near the end is useful for button visibility, but it must
          // not seize a manual gesture. Live-follow resumes only once the
          // scroller genuinely reaches the bottom.
          stickRef.current = distanceFromEnd <= BOTTOM_STICK_EPSILON;
          const present = distanceFromEnd < BOTTOM_SLACK;
          setAtPresent((prev) => (prev === present ? prev : present));
        }}
      >
        <div className="feed-inner" ref={feedInnerRef}>
          {state.feedLoading && <div className="feed-note note-status">loading log…</div>}
          {!state.feedLoading && feedLen === 0 && (
            <div className="feed-empty">
              <img
                src="/threadknot-logo.png"
                alt=""
                aria-hidden="true"
                className="feed-empty-icon"
              />
              <p>
                {draft
                  ? hermesHome
                    ? `Direct line to ${hermesGatewayName ?? "your Hermes agent"}. Say hello.`
                    : quickHome
                      ? "A private scratch conversation. Ask anything."
                      : `Fresh thread in ${project?.name ?? "project"}. Set your course below.`
                  : "No traffic on this channel yet."}
              </p>
            </div>
          )}
          {state.feed.map((item) => (
            <FeedItemView key={item.id} item={item} />
          ))}
          {busy && (
            <div className="working-row">
              <span className="sonar" />
              <span>
                {thread!.status === "waiting_approval"
                  ? "Holding for approval…"
                  : runningSubagents > 0
                    ? `Waiting on ${runningSubagents} child agent${runningSubagents === 1 ? "" : "s"}…`
                    : "Working…"}
              </span>
            </div>
          )}
        </div>
      </div>

      {/* Pinned above the composer (outside the scroll container) so it stays
          put on every layout — desktop, the stacked mobile panes, and zoom. */}
      <div className="composer-dock">
        {!atPresent && (
          <button
            type="button"
            className="jump-present"
            title="Jump to the newest activity"
            aria-label="Scroll to the newest activity"
            onClick={jumpToPresent}
          >
            <ArrowDownIcon size={13} />
            present
          </button>
        )}
        <AgentHud subagents={subagents} />
        <Composer thread={thread ?? null} />
      </div>
    </section>
  );
}

function EmptyPane() {
  const { dispatch, actions } = useStore();
  return (
    <section className="thread-pane empty-pane">
      <button
        className="icon-btn hamburger empty-hamburger"
        aria-label="Open sidebar"
        onClick={() => dispatch({ type: "sidebar", open: true })}
      >
        <MenuIcon size={18} />
      </button>
      <div className="empty-hero">
        <img src="/threadknot-logo.png" alt="Threadknot" className="empty-anchor-img" />
        <div className="empty-wordmark">THREADKNOT</div>
        <p className="empty-tag">every coding agent on one thread</p>
        <p className="empty-hint">
          Ask something now, or choose a workspace when the work belongs to a project.
        </p>
        <button
          type="button"
          className="empty-quick-action"
          onClick={() => actions.openQuickDraft()}
        >
          <span>New quick thread</span>
          <small>no workspace needed</small>
        </button>
      </div>
    </section>
  );
}
