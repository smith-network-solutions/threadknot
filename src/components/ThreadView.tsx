import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import {
  HERMES_HOME_PROJECT_ID,
  isQuickHomeProjectId,
  threadParticipants,
} from "../lib/protocol";
import { APPEARANCE_EVENT, getAppliedZoom } from "../lib/appearance";
import { findThread, resolveProjectView, useStore } from "../state/store";
import { useAvatarHoverPreview } from "./AvatarHoverPreview";
import { FeedItemView } from "./FeedItems";
import { ParleyRoundSplash, ReviewMenu } from "./ReviewMenu";
import { AgentHud } from "./AgentHud";
import { activeSubagents } from "../state/feed";
import { Composer } from "./Composer";
import {
  AgentMark,
  ArrowDownIcon,
  CheckIcon,
  MenuIcon,
  MoreIcon,
  PencilIcon,
} from "./icons";
import { VISIBLE_TABS } from "./WorkspacePanel";
import "../styles/workspace.css";

/** How close to the end we can be before showing the jump-to-present button. */
const BOTTOM_SLACK = 90;
/** Auto-follow only engages at the real end; a larger threshold makes manual
 * scrolling snap the final stretch and keeps tugging the reader back down. */
const BOTTOM_STICK_EPSILON = 2;
/** After any manual scroll gesture, the per-render position restore below is
 * suppressed for this long, so a streaming re-render can't yank the reader
 * back to scrollTopRef mid-scroll. Without it, scrolling during a live turn
 * fights the reader (onScroll lags the gesture, the restore wins the race). */
const GESTURE_GRACE_MS = 350;
/** Ignore sub-pixel/jitter differences when deciding to restore. Only a real
 * jump — the WebKit nested-scroller reset the restore exists for — clears it. */
const RESTORE_EPSILON = 8;

/** The header's workspace panels (Files, Git, Artifacts, Browser, Terminal)
 *  collapsed behind one "more" button — used on phones, where the desktop pills
 *  would overflow. Opening a row toggles that panel; the button lights up while
 *  any panel is open. Portaled + click-outside/Escape to close. */
function WorkspaceMenu({
  project,
  openTab,
  dispatch,
}: {
  project: { id: string } | undefined;
  openTab: string | null;
  dispatch: ReturnType<typeof useStore>["dispatch"];
}) {
  const [open, setOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);

  // Anchor under the button, right-aligned, clamped into the viewport. The
  // header renders unzoomed, so the rect maps 1:1 onto the fixed panel.
  useLayoutEffect(() => {
    if (!open) return;
    function place() {
      const b = btnRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      const width = 210;
      setPos({ top: r.bottom + 6, left: Math.max(8, r.right - width) });
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
      if (btnRef.current?.contains(t) || popRef.current?.contains(t)) return;
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

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        className={`head-pill${open || openTab ? " on" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Workspace panels"
        title="Workspace panels"
        disabled={!project}
        onClick={() => setOpen((v) => !v)}
      >
        <MoreIcon size={16} />
        <span>More</span>
      </button>
      {open && pos && project &&
        createPortal(
          <div
            ref={popRef}
            className="thread-menu ws-menu"
            role="menu"
            aria-label="Workspace panels"
            style={{ top: pos.top, left: pos.left }}
          >
            {VISIBLE_TABS.map(({ id, label, Icon }) => {
              const active = openTab === id;
              return (
                <button
                  key={id}
                  type="button"
                  role="menuitemradio"
                  aria-checked={active}
                  className={`thread-menu-item${active ? " on" : ""}`}
                  onClick={() => {
                    dispatch({
                      type: "workspace",
                      projectId: project.id,
                      tab: active ? null : id,
                    });
                    setOpen(false);
                  }}
                >
                  <Icon size={16} />
                  <span>{label}</span>
                  {active && (
                    <span className="ws-menu-check">
                      <CheckIcon size={15} />
                    </span>
                  )}
                </button>
              );
            })}
          </div>,
          document.body,
        )}
    </>
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
  const scrollHeightRef = useRef(0);
  const clientHeightRef = useRef(0);
  const touchYRef = useRef<number | null>(null);
  // Timestamp (ms) until which a manual scroll gesture is considered in flight.
  // While it's in the future, the per-render restore stands down (see below).
  const gestureUntilRef = useRef(0);
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
    } else if (
      Date.now() >= gestureUntilRef.current &&
      Math.abs(el.scrollTop - scrollTopRef.current) > RESTORE_EPSILON
    ) {
      // WebKit can reset a nested scroller when unrelated app state rerenders.
      // Restore the reader's last explicit position — but only for a real jump
      // and only when no manual scroll is in flight, or a streaming re-render
      // would fight the reader's own wheel/touch/drag mid-gesture.
      el.scrollTop = scrollTopRef.current;
    }
    scrollHeightRef.current = el.scrollHeight;
    clientHeightRef.current = el.clientHeight;
    setAtPresent((prev) => (prev === stickRef.current ? prev : stickRef.current));
  });

  const jumpToPresent = useCallback(() => {
    const el = scrollRef.current;
    if (!el) {
      stickRef.current = true;
      setAtPresent(true);
      return;
    }
    const reduceMotion =
      typeof window !== "undefined" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (reduceMotion) {
      stickRef.current = true;
      setAtPresent(true);
      el.scrollTop = el.scrollHeight;
      scrollTopRef.current = el.scrollTop;
      return;
    }
    // Glide to the newest activity instead of teleporting. Deliberately DON'T
    // arm live-follow here: the layout effect would then hard-pin to the bottom
    // on the next render and cancel the animation. onScroll re-arms stick and
    // hides this button on its own once the glide actually reaches the end.
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, []);

  // Growing content and a resizing composer both dispatch scroll events even
  // though the reader did not move the feed. Only an explicit gesture away
  // from the end may turn live-follow off; onScroll itself is still allowed to
  // turn it back on once a reader genuinely reaches the bottom.
  const leavePresent = useCallback(() => {
    stickRef.current = false;
  }, []);

  // Opens the grace window that keeps the per-render restore from overriding a
  // live scroll. Called from every genuine manual gesture (wheel, touch drag,
  // arrow/page keys, scrollbar drag) — not from streaming-driven scroll events.
  const markGesture = useCallback(() => {
    gestureUntilRef.current = Date.now() + GESTURE_GRACE_MS;
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
    // The scrollport also changes height when a long prompt collapses out of
    // the composer or the mobile keyboard moves. Keep following through those
    // viewport changes just as we do when the feed content itself grows.
    if (scrollRef.current) ro.observe(scrollRef.current);
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
              {agentInfo &&
                (hermesAvatar ? (
                  <span className="chip-avatar title-agent-mark" {...chipPreview.hoverProps}>
                    <img src={hermesAvatar} alt="" />
                  </span>
                ) : (
                  <AgentMark agent={agentInfo.id} size={15} className="title-agent-mark" />
                ))}
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
          {chipPreview.portal}
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
          {thread && <ParleyRoundSplash thread={thread} />}
          {!hermesHome && !quickHome && (
            <>
              {/* Desktop: one pill (icon + label) per panel. Hidden on phones. */}
              <div className="head-tab-pills">
                {VISIBLE_TABS.map(({ id, label, Icon }) => {
                  const openTab = project ? state.workspace[project.id] ?? null : null;
                  const active = openTab === id;
                  return (
                    <button
                      key={id}
                      type="button"
                      className={`head-pill${active ? " on" : ""}`}
                      aria-pressed={active}
                      title={label}
                      disabled={!project}
                      onClick={() =>
                        project &&
                        dispatch({
                          type: "workspace",
                          projectId: project.id,
                          tab: active ? null : id,
                        })
                      }
                    >
                      <Icon size={15} />
                      <span>{label}</span>
                    </button>
                  );
                })}
              </div>
              {/* Phones: the same panels collapsed into a dropdown. */}
              <div className="head-tab-menu">
                <WorkspaceMenu
                  project={project}
                  openTab={project ? state.workspace[project.id] ?? null : null}
                  dispatch={dispatch}
                />
              </div>
            </>
          )}
        </div>
      </header>

      <div
        className="feed-scroll"
        data-zoom-pane="feed"
        ref={scrollRef}
        onWheel={(e) => {
          markGesture();
          if (e.deltaY < 0) leavePresent();
        }}
        onTouchStart={(e) => {
          touchYRef.current = e.touches[0]?.clientY ?? null;
        }}
        onTouchMove={(e) => {
          markGesture();
          const previousY = touchYRef.current;
          const nextY = e.touches[0]?.clientY;
          if (previousY != null && nextY != null && nextY > previousY) {
            leavePresent();
          }
          touchYRef.current = nextY ?? null;
        }}
        onTouchEnd={() => {
          touchYRef.current = null;
        }}
        onTouchCancel={() => {
          touchYRef.current = null;
        }}
        onKeyDown={(e) => {
          const scrollKeys = [
            "ArrowUp",
            "PageUp",
            "Home",
            "ArrowDown",
            "PageDown",
            "End",
            " ",
          ];
          if (scrollKeys.includes(e.key)) markGesture();
          if (e.key === "ArrowUp" || e.key === "PageUp" || e.key === "Home") {
            leavePresent();
          }
        }}
        onScroll={(e) => {
          if (!loadedFeedId) return;
          const el = e.currentTarget;
          const previousTop = scrollTopRef.current;
          const geometryChanged =
            scrollHeightRef.current !== el.scrollHeight ||
            clientHeightRef.current !== el.clientHeight;
          scrollTopRef.current = el.scrollTop;
          scrollHeightRef.current = el.scrollHeight;
          clientHeightRef.current = el.clientHeight;
          const distanceFromEnd = Math.max(
            0,
            el.scrollHeight - el.scrollTop - el.clientHeight,
          );
          // Browser-generated scroll events also fire when streaming content,
          // the composer, or the mobile viewport changes size. Those must not
          // cancel live-follow. Explicit wheel/touch/key gestures above turn it
          // off; reaching the real end here is the only implicit state change.
          if (distanceFromEnd <= BOTTOM_STICK_EPSILON) {
            stickRef.current = true;
          } else if (!geometryChanged && el.scrollTop < previousTop - BOTTOM_STICK_EPSILON) {
            // Covers dragging the desktop scrollbar, which does not reliably
            // emit wheel or pointer events into the page. Treat it as a manual
            // gesture too, so the restore doesn't fight a scrollbar drag.
            markGesture();
            leavePresent();
          }
          const present = distanceFromEnd < BOTTOM_SLACK;
          setAtPresent((prev) => (prev === present ? prev : present));
        }}
      >
        <div className="feed-inner" ref={feedInnerRef}>
          {state.feedLoading && <div className="feed-note note-status">loading log…</div>}
          {!state.feedLoading && feedLen === 0 && (
            <div className="feed-empty">
              <img
                src="/threadknot-simple.png"
                alt=""
                aria-hidden="true"
                className="feed-empty-logo"
              />
              <p>
                {draft
                  ? hermesHome
                    ? `Say hello to ${hermesGatewayName ?? "your Hermes agent"}`
                    : quickHome
                      ? "Ask anything…"
                      : "Let's get started…"
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
            Present
          </button>
        )}
        <AgentHud
          subagents={subagents}
          onOpenThread={(threadId) => void actions.selectThread(threadId)}
        />
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
