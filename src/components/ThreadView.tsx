import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import {
  HERMES_HOME_PROJECT_ID,
  isQuickHomeProjectId,
  threadParticipants,
  type Thread,
  type ThreadSettings,
} from "../lib/protocol";
import { APPEARANCE_EVENT, getAppliedZoom } from "../lib/appearance";
import { elidePathMiddle } from "../lib/format";
import type { ReplyTarget } from "../lib/reply";
import type { FeedItem } from "../state/feed";
import {
  effortForModel,
  findThread,
  rememberNewThreadSettings,
  remoteMachineId,
  resolveProjectView,
  useStore,
  useFeedStore,
} from "../state/store";
import { useAvatarHoverPreview } from "./AvatarHoverPreview";
import { FeedItemView, thoughtTimesForFeed, type FeedRenderContext } from "./FeedItems";
import { ParleyRoundSplash, ReviewDialog, ReviewMenu, useReviewBlock } from "./ReviewMenu";
import { AgentHud } from "./AgentHud";
import { activeSubagents } from "../state/feed";
import { Composer } from "./Composer";
import {
  AgentMark,
  ArrowDownIcon,
  CheckIcon,
  ChevronIcon,
  MenuIcon,
  MoreIcon,
  PanelLeftIcon,
  PencilIcon,
  SearchIcon,
  ShieldIcon,
  XIcon,
} from "./icons";
import { VISIBLE_TABS } from "./WorkspacePanel";
import "../styles/workspace.css";

/** How close to the end we can be before showing the jump-to-present button. */
const BOTTOM_SLACK = 90;
/** Auto-follow only engages at the real end; a larger threshold makes manual
 * scrolling snap the final stretch and keeps tugging the reader back down. */
const BOTTOM_STICK_EPSILON = 2;
/** How far from the end a drop in scrollTop must leave the reader before it
 * counts as them scrolling up.
 *
 * A drop is not proof of intent. The browser drops scrollTop itself, by
 * clamping, whenever the end moves up under a reader sitting on it — which is
 * what every shrink of the composer dock does: an agent pill going away after a
 * tool call, the composer closing around a sent image, the phone keyboard
 * retracting. Those clamps land the reader exactly on the new end; a real
 * gesture leaves them off it. So the distance afterwards is what separates the
 * two, and it only has to clear rounding and sub-pixel geometry — well under
 * the smallest deliberate scroll anyone makes. */
const CLAMP_SLACK = 24;
/** Keep the normal transcript DOM bounded. Older pages are prepended before the
 * reader reaches the top, with the visible position restored in layout. */
const INITIAL_FEED_ROWS = 160;
const FEED_PAGE_ROWS = 80;

function feedSearchText(item: FeedItem): string {
  switch (item.type) {
    case "user":
    case "assistant":
    case "thinking":
    case "note":
      return item.text;
    case "tool":
      return `${item.name}\n${item.detail}\n${item.output}`;
    case "diff":
      return `${item.path}\n${item.unified}`;
    case "artifact":
      return `${item.name}\n${item.relPath}\n${item.description ?? ""}`;
    case "approval":
      return `${item.title}\n${item.detail}`;
    case "failure":
      return `${item.title}\n${item.message}\n${item.path ?? ""}\n${item.hint ?? ""}`;
    case "question":
      return JSON.stringify(item.questions) + JSON.stringify(item.answers);
    case "turn":
      return `${item.agent ?? ""} ${item.model ?? ""}`;
    case "turn_end":
    case "context_usage":
      return "";
  }
}

/** High-frequency token frames replace only the final text item. Derived feed
 * metadata is structural, so rescanning the entire transcript for those frames
 * burns scroll time without changing the answer. */
function isTailTextUpdate(previous: FeedItem[], next: FeedItem[]): boolean {
  if (previous.length === 0 || previous.length !== next.length) return false;
  const last = next.length - 1;
  if (last > 0 && previous[last - 1] !== next[last - 1]) return false;
  const before = previous[last];
  const after = next[last];
  return (
    before !== after &&
    before.id === after.id &&
    before.type === after.type &&
    (after.type === "assistant" || after.type === "thinking")
  );
}

const FeedFindItem = memo(function FeedFindItem({
  item,
  matched,
  current,
  itemRef,
  render,
  thoughtTime,
}: {
  item: FeedItem;
  matched: boolean;
  current: boolean;
  itemRef: (node: HTMLDivElement | null) => void;
  render: FeedRenderContext;
  thoughtTime?: string;
}) {
  return (
    <div
      ref={itemRef}
      className={`feed-find-item${matched ? " is-match" : ""}${current ? " is-current" : ""}`}
    >
      <FeedItemView item={item} render={render} thoughtTime={thoughtTime} />
    </div>
  );
});

function ChatFindBar({
  inputRef,
  query,
  onQueryChange,
  matchCount,
  matchIndex,
  onPrevious,
  onNext,
  onClose,
}: {
  inputRef: RefObject<HTMLInputElement>;
  query: string;
  onQueryChange: (value: string) => void;
  matchCount: number;
  matchIndex: number;
  onPrevious: () => void;
  onNext: () => void;
  onClose: () => void;
}) {
  return (
    <div className="chat-find" role="search" aria-label="Find in chat">
      <SearchIcon size={15} className="chat-find-icon" />
      <input
        ref={inputRef}
        type="search"
        value={query}
        placeholder="Find in this chat…"
        aria-label="Find in this chat"
        onChange={(event) => onQueryChange(event.target.value)}
        spellCheck={false}
      />
      <span className="chat-find-count" aria-live="polite">
        {query.trim() ? (matchCount > 0 ? `${matchIndex + 1} of ${matchCount}` : "No matches") : "Find"}
      </span>
      <button
        type="button"
        className="chat-find-nav"
        aria-label="Previous match"
        title="Previous match"
        disabled={matchCount === 0}
        onClick={onPrevious}
      >
        <ChevronIcon size={14} className="chat-find-prev" />
      </button>
      <button
        type="button"
        className="chat-find-nav"
        aria-label="Next match"
        title="Next match"
        disabled={matchCount === 0}
        onClick={onNext}
      >
        <ChevronIcon size={14} className="chat-find-next" />
      </button>
      <button
        type="button"
        className="chat-find-close"
        aria-label="Close find"
        title="Close find (Escape)"
        onClick={onClose}
      >
        <XIcon size={14} />
      </button>
    </div>
  );
}

/** The header's workspace panels (Files, Git, Artifacts, Browser, Terminal)
 *  plus Review, collapsed behind one icon-only button — used on phones, where
 *  the desktop pills would overflow. Opening a row toggles that panel; the
 *  button lights up while any panel is open. Portaled + click-outside/Escape
 *  to close. */
function WorkspaceMenu({
  project,
  openTab,
  dispatch,
  thread,
}: {
  project: { id: string } | undefined;
  openTab: string | null;
  dispatch: ReturnType<typeof useStore>["dispatch"];
  thread: Thread | null;
}) {
  const [open, setOpen] = useState(false);
  const [reviewOpen, setReviewOpen] = useState(false);
  const reviewBlocked = useReviewBlock(thread);
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
      </button>
      {reviewOpen && thread && (
        <ReviewDialog thread={thread} onClose={() => setReviewOpen(false)} />
      )}
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
            {/* Review lives here on phones rather than as its own header pill —
                the header only has room for the one cluster. `divided` sets it
                off from the panel toggles above: it opens a dialog, it doesn't
                toggle a panel. */}
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item divided"
              disabled={!!reviewBlocked}
              title={reviewBlocked ?? "Review with another agent"}
              onClick={() => {
                setOpen(false);
                setReviewOpen(true);
              }}
            >
              <ShieldIcon size={16} />
              <span>Review</span>
            </button>
          </div>,
          document.body,
        )}
    </>
  );
}

/** The new-thread empty state is also the moment a person chooses the folder
 * they want an agent to work in. Keep that choice in the sentence itself,
 * rather than making it a separate piece of chrome above the composer. */
function WorkOnProject({
  project,
  agent,
  settings,
}: {
  project: { id: string; name: string };
  agent: string;
  settings: ThreadSettings;
}) {
  const { state, dispatch, actions } = useStore();
  const [open, setOpen] = useState(false);
  const [modelOpen, setModelOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);
  const modelRef = useRef<HTMLDivElement>(null);
  const agentInfo = state.hello?.agents.find((candidate) => candidate.id === agent);
  const currentModel = agentInfo?.models.find((model) => model.id === settings.model);
  const agentModels = (state.hello?.agents ?? []).flatMap((candidate) =>
    candidate.available || candidate.id === agent
      ? candidate.models.map((model) => ({ agent: candidate, model }))
      : [],
  );
  const choices = useMemo(() => {
    const byId = new Map<string, { id: string; name: string; machineId?: string }>();

    for (const candidate of state.projects) {
      byId.set(candidate.id, { id: candidate.id, name: candidate.name });
    }
    // A fleet view can be working in a root that only exists on a paired
    // machine. Include those alongside local projects so the switcher does
    // not strand the person on the local machine.
    for (const workspace of state.workspaces) {
      for (const member of workspace.members) {
        const view = resolveProjectView(state, member.projectId);
        if (view && !byId.has(view.project.id)) {
          byId.set(view.project.id, {
            id: view.project.id,
            name: view.project.name,
            machineId: view.machineId,
          });
        }
      }
    }
    if (!byId.has(project.id)) {
      const view = resolveProjectView(state, project.id);
      byId.set(project.id, {
        id: project.id,
        name: project.name,
        machineId: view?.machineId,
      });
    }
    return [...byId.values()];
  }, [state.projects, state.workspaces, state.hello?.machineId, project]);

  useEffect(() => {
    if (!open) return;
    function onPointerDown(event: PointerEvent) {
      if (!pickerRef.current?.contains(event.target as Node)) setOpen(false);
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setOpen(false);
    }
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  useEffect(() => {
    if (!modelOpen) return;
    function onPointerDown(event: PointerEvent) {
      if (!modelRef.current?.contains(event.target as Node)) setModelOpen(false);
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setModelOpen(false);
    }
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [modelOpen]);

  const selectProject = (choice: (typeof choices)[number]) => {
    setOpen(false);
    actions.openDraft(choice.id, choice.machineId);
  };

  return (
    <p className="empty-work-on">
      <span className="empty-work-on-prefix">Let's work on</span>
      <span className="empty-project-picker" ref={pickerRef}>
        <button
          type="button"
          className="empty-project-trigger"
          aria-haspopup="menu"
          aria-expanded={open}
          aria-label={`Change project, currently ${project.name}`}
          onClick={() => {
            setModelOpen(false);
            setOpen((wasOpen) => !wasOpen);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              setOpen(true);
            }
          }}
        >
          <span className="empty-project-label">{project.name}</span>
          <ChevronIcon open size={14} className="empty-project-chevron" />
        </button>
        {open && (
          <span className="empty-project-menu" role="menu" aria-label="Choose project">
          {choices.map((choice) => (
            <button
              key={choice.id}
              type="button"
              role="menuitemradio"
              aria-checked={choice.id === project.id}
              className={`empty-project-option${choice.id === project.id ? " selected" : ""}`}
              onClick={() => selectProject(choice)}
            >
              {choice.name}
            </button>
          ))}
          </span>
        )}
      </span>
      <span className="empty-model-picker" ref={modelRef}>
        <button
          type="button"
          className="empty-model-trigger"
          aria-haspopup="menu"
          aria-expanded={modelOpen}
          aria-label={`Change AI or model, currently ${agentInfo?.name ?? agent} ${currentModel?.name ?? settings.model}`}
          onClick={() => {
            setOpen(false);
            setModelOpen((wasOpen) => !wasOpen);
          }}
        >
          <span className="empty-model-using">Using</span>
          <AgentMark agent={agent} size={13} />
          <span>{agentInfo?.name ?? agent}</span>
          <span className="empty-model-name">{currentModel?.name ?? settings.model}</span>
          <ChevronIcon open size={12} className="empty-model-chevron" />
        </button>
        {modelOpen && (
          <span className="empty-model-menu" role="menu" aria-label="Choose AI and model">
            {agentModels.map(({ agent: candidate, model }) => (
              <button
                key={`${candidate.id}:${model.id}`}
                type="button"
                role="menuitemradio"
                aria-checked={candidate.id === agent && model.id === settings.model}
                className={`empty-model-option${candidate.id === agent && model.id === settings.model ? " selected" : ""}`}
                onClick={() => {
                  setModelOpen(false);
                  const changingAgent = candidate.id !== agent;
                  const nextSettings = {
                    ...settings,
                    model: model.id,
                    effort: effortForModel(
                      model,
                      changingAgent ? undefined : settings.effort,
                      candidate.id === "claude",
                    ),
                    wideContext:
                      !changingAgent && model.supportsWideContext && candidate.id === "claude"
                        ? settings.wideContext
                        : undefined,
                    claudeChrome: changingAgent ? undefined : settings.claudeChrome,
                  };
                  // This is a draft-only surface, so commit the complete pair
                  // at once instead of first selecting an agent's default
                  // model and then replacing it in a second render.
                  dispatch({ type: "draftSettings", agent: candidate.id, settings: nextSettings });
                  if (state.draft) {
                    rememberNewThreadSettings(state.draft.projectId, candidate.id, nextSettings);
                  }
                }}
              >
                <AgentMark agent={candidate.id} size={13} />
                <span className="empty-model-option-agent">{candidate.name}</span>
                <span className="empty-model-option-name">{model.name}</span>
              </button>
            ))}
          </span>
        )}
      </span>
    </p>
  );
}

export function ThreadView() {
  const { state, dispatch, actions } = useFeedStore();
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
  const projectDraft = !!draft && !!project && !hermesHome && !quickHome;
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

  // Which machine this chat actually runs on. The sidebar row can't say it on a
  // phone (its machine chip costs more width than the title has), and the
  // native server switcher names the server you dialled, not the mesh machine
  // that owns the thread — those differ the moment a workspace spans two boxes.
  // So the header carries it, next to the directory it belongs with. Named for
  // local threads too: "which box is this path on" is the question, and
  // answering it only for remote chats leaves the common case guessing.
  const threadMachineId = thread?.machineId ?? draft?.machineId;
  const localMachineId = state.hello?.machineId;
  const deviceIsRemote =
    !!threadMachineId && !!localMachineId && threadMachineId !== localMachineId;
  const devicePeer = deviceIsRemote
    ? state.peers.find((p) => p.machineId === threadMachineId)
    : undefined;
  const deviceName = deviceIsRemote
    ? (devicePeer?.name ?? "remote machine")
    : (state.hello?.friendlyName ?? state.hello?.serverName ?? "this machine");
  const deviceColor = deviceIsRemote
    ? (devicePeer?.colorOverride ?? devicePeer?.color)
    : state.hello?.color;
  const deviceOffline = deviceIsRemote && !devicePeer?.online;

  const [editing, setEditing] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [quickMode, setQuickMode] = useState<"chat" | "build">("chat");
  const [replyTo, setReplyTo] = useState<ReplyTarget | null>(null);
  const [findOpen, setFindOpen] = useState(false);
  const [findPresent, setFindPresent] = useState(false);
  const [findClosing, setFindClosing] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findIndex, setFindIndex] = useState(0);
  const findInputRef = useRef<HTMLInputElement>(null);
  const findItemRefs = useRef<Record<string, HTMLDivElement | null>>({});
  const findItemRefCallbacks = useRef<
    Record<string, (node: HTMLDivElement | null) => void>
  >({});
  const findCloseTimerRef = useRef<number | null>(null);

  const scrollRef = useRef<HTMLDivElement>(null);
  const stickRef = useRef(true);
  // Upward intent is latched. Near-bottom geometry alone must never re-arm
  // live-follow after the reader has started moving into history.
  const manualAwayRef = useRef(false);
  const scrollTopRef = useRef(0);
  const scrollHeightRef = useRef(0);
  const clientHeightRef = useRef(0);
  const touchYRef = useRef<number | null>(null);
  // The zoom scrollTopRef's number was measured under. Compared (not just used)
  // so one zoom value is only ever rescaled once (see the APPEARANCE_EVENT
  // handler below).
  const zoomRef = useRef(getAppliedZoom());
  const scrollFeedRef = useRef<string | null>(null);
  const loadedFeedId =
    state.activeThreadId && state.feedThreadId === state.activeThreadId
      ? state.activeThreadId
      : null;

  useEffect(() => {
    setReplyTo(null);
  }, [loadedFeedId]);

  const findMatches = useMemo(() => {
    const q = findQuery.trim().toLowerCase();
    if (!q) return [];
    return state.feed.filter((item) => feedSearchText(item).toLowerCase().includes(q));
  }, [findQuery, state.feed]);
  const activeFindIndex = findMatches.length > 0
    ? Math.min(findIndex, findMatches.length - 1)
    : 0;
  const activeFindId = findMatches[activeFindIndex]?.id;
  const findMatchIds = useMemo(
    () => new Set(findMatches.map((item) => item.id)),
    [findMatches],
  );
  const findItemRef = useCallback((id: string) => {
    const existing = findItemRefCallbacks.current[id];
    if (existing) return existing;
    const callback = (node: HTMLDivElement | null) => {
      findItemRefs.current[id] = node;
    };
    findItemRefCallbacks.current[id] = callback;
    return callback;
  }, []);

  useEffect(() => {
    findItemRefs.current = {};
    findItemRefCallbacks.current = {};
  }, [loadedFeedId]);

  const openFind = useCallback(() => {
    if (findCloseTimerRef.current !== null) {
      window.clearTimeout(findCloseTimerRef.current);
      findCloseTimerRef.current = null;
    }
    setFindPresent(true);
    setFindClosing(false);
    setFindOpen(true);
  }, []);

  const closeFind = useCallback(() => {
    setFindOpen(false);
    setFindClosing(true);
    if (findCloseTimerRef.current !== null) window.clearTimeout(findCloseTimerRef.current);
    findCloseTimerRef.current = window.setTimeout(() => {
      findCloseTimerRef.current = null;
      setFindPresent(false);
      setFindClosing(false);
    }, 180);
  }, []);

  // The browser's native find UI is not consistently available in the desktop
  // webview. Keep the shortcut scoped to the chat unless it was pressed over a
  // workspace panel, where that panel owns the search interaction.
  useEffect(() => {
    function onFind(event: KeyboardEvent) {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "f") return;
      const target = event.target as HTMLElement | null;
      if (target?.closest(".workspace-panel, [role='dialog']")) return;
      event.preventDefault();
      openFind();
      setFindIndex(0);
    }
    window.addEventListener("keydown", onFind);
    return () => window.removeEventListener("keydown", onFind);
  }, [openFind]);

  useEffect(() => {
    if (!findOpen) return;
    findInputRef.current?.focus();
    findInputRef.current?.select();
    function onEscape(event: KeyboardEvent) {
      if (event.key === "Escape") closeFind();
    }
    window.addEventListener("keydown", onEscape);
    return () => window.removeEventListener("keydown", onEscape);
  }, [closeFind, findOpen]);

  useEffect(() => () => {
    if (findCloseTimerRef.current !== null) window.clearTimeout(findCloseTimerRef.current);
  }, []);

  useEffect(() => {
    if (!findOpen || !activeFindId) return;
    const frame = window.requestAnimationFrame(() => {
      const match = findItemRefs.current[activeFindId];
      const feed = scrollRef.current;
      if (!match || !feed) return;
      // Use the feed's own scrollport explicitly. This avoids the browser
      // choosing the document/window as the nearest scroller when the find
      // control is sticky inside the conversation.
      const matchRect = match.getBoundingClientRect();
      const feedRect = feed.getBoundingClientRect();
      const targetTop = feed.scrollTop + matchRect.top - feedRect.top
        - (feed.clientHeight - matchRect.height) / 2;
      feed.scrollTo({
        top: Math.max(0, targetTop),
        behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
      });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [activeFindId, findOpen, findQuery]);
  const observerRef = useRef<ResizeObserver | null>(null);
  const pinRafRef = useRef<number | null>(null);
  const scrollMetricsRafRef = useRef<number | null>(null);
  const scrollIdleTimerRef = useRef<number | null>(null);
  const prependAnchorRef = useRef<{ scrollHeight: number; scrollTop: number } | null>(null);
  const [atPresent, setAtPresent] = useState(true);
  const feedLen = state.feed.length;
  const [feedWindow, setFeedWindow] = useState<{ feedId: string | null; start: number }>({
    feedId: null,
    start: 0,
  });
  const initialFeedStart = Math.max(0, feedLen - INITIAL_FEED_ROWS);
  const feedStart = findOpen
    ? 0
    : feedWindow.feedId === loadedFeedId
      ? Math.min(feedWindow.start, initialFeedStart)
      : initialFeedStart;

  useEffect(() => {
    prependAnchorRef.current = null;
    setFeedWindow({ feedId: loadedFeedId, start: initialFeedStart });
  }, [loadedFeedId]);

  const prependOlderFeed = useCallback((el: HTMLDivElement) => {
    if (!loadedFeedId || findOpen || feedStart <= 0 || prependAnchorRef.current) return;
    prependAnchorRef.current = {
      scrollHeight: el.scrollHeight,
      scrollTop: el.scrollTop,
    };
    setFeedWindow({
      feedId: loadedFeedId,
      start: Math.max(0, feedStart - FEED_PAGE_ROWS),
    });
  }, [feedStart, findOpen, loadedFeedId]);

  useLayoutEffect(() => {
    const anchor = prependAnchorRef.current;
    const el = scrollRef.current;
    if (!anchor || !el) return;
    prependAnchorRef.current = null;
    const nextTop = anchor.scrollTop + (el.scrollHeight - anchor.scrollHeight);
    el.scrollTop = nextTop;
    scrollTopRef.current = nextTop;
  }, [feedStart]);
  const feedDerivedRef = useRef<{
    feed: FeedItem[];
    subagents: ReturnType<typeof activeSubagents>;
    thoughtTimes: Record<string, string>;
  } | null>(null);
  const feedDerived = useMemo(() => {
    const previous = feedDerivedRef.current;
    if (previous && isTailTextUpdate(previous.feed, state.feed)) {
      const reused = { ...previous, feed: state.feed };
      feedDerivedRef.current = reused;
      return reused;
    }
    const next = {
      feed: state.feed,
      subagents: activeSubagents(state.feed),
      thoughtTimes: thoughtTimesForFeed(state.feed),
    };
    feedDerivedRef.current = next;
    return next;
  }, [state.feed]);
  const subagents = feedDerived.subagents;
  const runningSubagents = subagents.filter((subagent) => subagent.status === "running").length;
  const feedParticipants = useMemo(
    () => (thread ? threadParticipants(thread) : []),
    [thread?.participants, thread?.agent, thread?.settings],
  );
  const thoughtTimes = feedDerived.thoughtTimes;
  const selectReply = useCallback((target: ReplyTarget) => {
    setReplyTo(target);
  }, []);
  const openFile = useCallback(
    (path: string) => {
      if (!project?.id) return;
      dispatch({ type: "workspace", projectId: project.id, tab: "files" });
      dispatch({ type: "fileFocus", projectId: project.id, path });
    },
    [dispatch, project?.id],
  );
  const feedRender = useMemo<FeedRenderContext>(
    () => ({
      threadId: loadedFeedId,
      projectId: project?.id ?? null,
      machineId: thread ? remoteMachineId(state, thread.machineId) : undefined,
      http: state.http,
      project: project ?? undefined,
      gitRepos: project ? state.git[project.id] : undefined,
      participants: feedParticipants,
      dispatch,
      actions,
      onReply: selectReply,
      onOpenFile: openFile,
    }),
    [
      loadedFeedId,
      project,
      thread?.machineId,
      state.http,
      project ? state.git[project.id] : undefined,
      feedParticipants,
      dispatch,
      actions,
      selectReply,
      openFile,
    ],
  );

  const pinToEnd = useCallback(() => {
    if (pinRafRef.current !== null) return;
    pinRafRef.current = window.requestAnimationFrame(() => {
      pinRafRef.current = null;
      const el = scrollRef.current;
      if (!el || !stickRef.current) return;
      const end = Math.max(0, el.scrollHeight - el.clientHeight);
      // Both halves are required. A drop alone is what the browser does when it
      // clamps a reader who is on the end onto a new, higher end (see
      // CLAMP_SLACK) — releasing live-follow there is what left the feed stuck
      // mid-history after a pill vanished. Distance alone is what growing
      // content does every time a token lands, which is the case this whole
      // callback exists to follow.
      if (
        el.scrollTop < scrollTopRef.current - BOTTOM_STICK_EPSILON &&
        end - el.scrollTop > CLAMP_SLACK
      ) {
        manualAwayRef.current = true;
        stickRef.current = false;
        scrollTopRef.current = el.scrollTop;
        return;
      }
      if (Math.abs(el.scrollTop - end) > 1) el.scrollTop = end;
      scrollTopRef.current = el.scrollTop;
      scrollHeightRef.current = el.scrollHeight;
      clientHeightRef.current = el.clientHeight;
    });
  }, []);

  // Feed updates can arrive many times per second. Keep this effect free of
  // synchronous scrollHeight/clientHeight reads: the pin callback below runs
  // once in the browser's paint cycle and is shared with ResizeObserver.
  useEffect(() => {
    if (!loadedFeedId) return;
    // An empty/new thread has no history to jump back from. Reset any stale
    // scroll state left by the previous thread while the empty state is shown.
    if (feedLen === 0) {
      stickRef.current = true;
      manualAwayRef.current = false;
      scrollTopRef.current = 0;
      setAtPresent(true);
      return;
    }
    if (scrollFeedRef.current !== loadedFeedId) {
      scrollFeedRef.current = loadedFeedId;
      stickRef.current = true;
      manualAwayRef.current = false;
      scrollTopRef.current = scrollRef.current?.scrollTop ?? 0;
      setAtPresent(true);
    }
    if (stickRef.current) pinToEnd();
  }, [loadedFeedId, feedLen, state.feed, pinToEnd]);

  const jumpToPresent = useCallback(() => {
    const el = scrollRef.current;
    manualAwayRef.current = false;
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
      pinToEnd();
      return;
    }
    // Glide to the newest activity instead of teleporting. Deliberately DON'T
    // arm live-follow here: the layout effect would then hard-pin to the bottom
    // on the next render and cancel the animation. onScroll re-arms stick and
    // hides this button on its own once the glide actually reaches the end.
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  }, [pinToEnd]);

  // Growing content and a resizing composer both dispatch scroll events even
  // though the reader did not move the feed. Only explicit movement controls
  // live-follow: away latches it off, toward the present permits it to re-arm
  // once the real bottom is reached.
  const leavePresent = useCallback(() => {
    manualAwayRef.current = true;
    stickRef.current = false;
    // Feed/resize work may already have queued a bottom pin for this frame.
    // Cancel it at gesture time so it cannot race the native wheel movement.
    if (pinRafRef.current !== null) {
      window.cancelAnimationFrame(pinRafRef.current);
      pinRafRef.current = null;
    }
  }, []);

  const markUserScrolling = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.classList.add("is-user-scrolling");
    if (scrollIdleTimerRef.current !== null) {
      window.clearTimeout(scrollIdleTimerRef.current);
    }
    scrollIdleTimerRef.current = window.setTimeout(() => {
      scrollIdleTimerRef.current = null;
      el.classList.remove("is-user-scrolling");
    }, 120);
  }, []);

  // Capture wheel intent directly on the scrollport. React's delegated wheel
  // handler runs later in propagation; on a rapidly streaming feed that leaves
  // a window for a queued resize pin to land before the synthetic callback.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = (event: WheelEvent) => {
      markUserScrolling();
      if (event.deltaY < 0) leavePresent();
      else if (event.deltaY > 0) manualAwayRef.current = false;
    };
    el.addEventListener("wheel", onWheel, { capture: true, passive: true });
    return () => el.removeEventListener("wheel", onWheel, true);
  }, [leavePresent, loadedFeedId, markUserScrolling]);

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
      pinToEnd();
    });
    ro.observe(node);
    observerRef.current = ro;
  }, [pinToEnd]);

  // Where the end *is* depends on the scrollport's height as much as on the
  // content's, and that height changes without the feed changing at all: the
  // dock above the composer grows when an agent pill appears after a tool call
  // and shrinks when it goes, the composer opens around a pasted image, the
  // phone keyboard slides in. Every one of those moves the end out from under a
  // reader who was following it, and none of them resizes the feed content, so
  // the observer above never hears about them.
  //
  // This used to be one more `ro.observe()` inside the feed-inner ref callback,
  // where it silently did nothing: React attaches a child's ref before its
  // parent's, so `scrollRef.current` was still null every time that callback
  // ran. A layout effect runs after every ref in the tree is set, which is the
  // only place this can be wired from and be sure of what it is observing.
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (!stickRef.current) return;
      pinToEnd();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [pinToEnd]);

  // On phones the header floats over the feed (see the mobile .thread-head
  // rules), so the feed has to reserve its height itself. That height is not a
  // constant: a multi-lane thread wraps the header to as many as four rows, and
  // the safe-area inset changes on rotation. Measure it and publish it as
  // --head-h on .thread-pane, which the mobile .feed-inner padding reads.
  //
  // The write goes straight to the DOM rather than through state: this fires on
  // every header reflow, and a re-render per reflow would show up in
  // docs/RENDER-FORENSICS.md as the whole thread flashing.
  const headObserverRef = useRef<ResizeObserver | null>(null);
  const threadHeadRef = useCallback((node: HTMLElement | null) => {
    headObserverRef.current?.disconnect();
    headObserverRef.current = null;
    if (!node) return;
    const pane = node.parentElement;
    if (!pane) return;
    const ro = new ResizeObserver(() => {
      pane.style.setProperty("--head-h", `${Math.round(node.getBoundingClientRect().height)}px`);
    });
    ro.observe(node);
    headObserverRef.current = ro;
  }, []);

  // The composer floats over the foot of the feed, so the feed reserves room
  // for it — and that room is not a constant either. The card grows with every
  // wrapped line of a long prompt, and with attachments, queued follow-ups and
  // the reply strip on top of that. Reserved as a fixed 132px, anything taller
  // simply covered the end of the conversation, which is the one part you are
  // usually reading. Measured and published as --dock-h on .thread-pane, the
  // same way (and for the same reason) as --head-h above.
  //
  // Written straight to the DOM rather than through state, for the same reason:
  // this fires on every keystroke that wraps a line, and a re-render each time
  // would show up in docs/RENDER-FORENSICS.md as the whole thread flashing.
  const dockObserverRef = useRef<ResizeObserver | null>(null);
  const composerDockRef = useCallback((node: HTMLDivElement | null) => {
    dockObserverRef.current?.disconnect();
    dockObserverRef.current = null;
    if (!node) return;
    const pane = node.parentElement;
    if (!pane) return;
    const ro = new ResizeObserver(() => {
      pane.style.setProperty("--dock-h", `${Math.round(node.getBoundingClientRect().height)}px`);
      // Growing the composer eats the scrollport from the bottom. Someone
      // reading the newest message has to stay on it rather than watch it slide
      // under the card they are typing into.
      if (stickRef.current) pinToEnd();
    });
    ro.observe(node);
    dockObserverRef.current = ro;
  }, [pinToEnd]);

  useEffect(
    () => () => {
      observerRef.current?.disconnect();
      headObserverRef.current?.disconnect();
      dockObserverRef.current?.disconnect();
      if (pinRafRef.current !== null) window.cancelAnimationFrame(pinRafRef.current);
      if (scrollMetricsRafRef.current !== null) {
        window.cancelAnimationFrame(scrollMetricsRafRef.current);
      }
      if (scrollIdleTimerRef.current !== null) {
        window.clearTimeout(scrollIdleTimerRef.current);
      }
      scrollRef.current?.classList.remove("is-user-scrolling");
    },
    [],
  );

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
  }, [pinToEnd]);

  const busy = thread && thread.status !== "idle";
  // Multi-lane threads get a two-row header on phones (the lane roster drops
  // to its own scrolling strip) — see the mobile `.thread-head.has-lanes` CSS.
  const hasLanes = !!thread && threadParticipants(thread).length > 1;

  // Navigation normally establishes a draft before this view paints. During a
  // brief boot/navigation handoff, render the pane shell instead of reviving
  // the old center-screen "New quick thread" gate.
  if (!thread && !draft) {
    return (
      <section className="thread-pane empty-pane">
        <button
          className="icon-btn hamburger empty-hamburger"
          aria-label="Open sidebar"
          onClick={() => dispatch({ type: "sidebar", open: true })}
        >
          <MenuIcon size={18} />
        </button>
      </section>
    );
  }

  return (
    <section className="thread-pane">
      <header
        ref={threadHeadRef}
        className={`thread-head${hasLanes ? " has-lanes" : ""}${quickHome ? " quick-thread-head" : ""}`}
      >
        {/* The phone header's one control cluster: sidebar + More share a
            single capsule. `display: contents` on desktop, where the hamburger
            is hidden and the panels are pills instead. */}
        <div className="head-cluster">
          <button
            className="icon-btn hamburger"
            aria-label="Open sidebar"
            onClick={() => dispatch({ type: "sidebar", open: true })}
          >
            {/* The same glyph the desktop sidebar collapse button uses — this
                opens that panel, so it should not read as a generic menu. */}
            <PanelLeftIcon size={18} />
          </button>
          {!hermesHome && !quickHome && (
            <div className="head-tab-menu">
              <WorkspaceMenu
                project={project}
                openTab={project ? state.workspace[project.id] ?? null : null}
                dispatch={dispatch}
                thread={thread}
              />
            </div>
          )}
        </div>
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
              <span
                className="thread-name"
                title={thread ? thread.title || "Untitled thread" : undefined}
              >
                {thread
                  ? thread.title || "Untitled thread"
                  : quickHome
                    ? "New quick thread"
                    : "New thread"}
              </span>
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
          {project?.path && !hermesHome && !quickHome && (
            /* A plain block on desktop, where the chip is hidden and this
               renders exactly as the bare path always did; a flex row on
               phones, where the chip leads and the path truncates after it. */
            <div className="thread-head-meta">
              <span
                className={`device-chip${deviceOffline ? " off" : ""}`}
                title={
                  deviceIsRemote
                    ? `Runs on ${deviceName}${deviceOffline ? " (offline)" : ""}`
                    : `Runs on this machine (${deviceName})`
                }
              >
                {deviceColor && (
                  <span
                    className="device-chip-dot"
                    style={{ background: deviceColor }}
                    aria-hidden
                  />
                )}
                {deviceName}
              </span>
              <span
                className="thread-project-path"
                title={project.path}
                aria-label={`Project directory: ${project.path}`}
              >
                {project.path}
              </span>
            </div>
          )}
          {chipPreview.portal}
        </div>
        {quickHome && (
          <div
            className={`quick-mode-tabs mode-${quickMode}`}
            role="tablist"
            aria-label="Quick thread mode"
          >
            <span className="quick-mode-indicator" aria-hidden="true" />
            <button
              type="button"
              role="tab"
              aria-selected={quickMode === "chat"}
              className={quickMode === "chat" ? "active" : ""}
              onClick={() => setQuickMode("chat")}
            >
              Chat
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={quickMode === "build"}
              className={quickMode === "build" ? "active" : ""}
              onClick={() => setQuickMode("build")}
            >
              Build
            </button>
          </div>
        )}
        {busy && (
          <div className="head-status">
            <span className="sonar" />
            {thread!.parley
              ? `parley · round ${thread!.parley.round}/${thread!.parley.maxRounds}`
              : thread!.status === "waiting_approval"
                ? "awaiting approval"
                : null}
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
            </>
          )}
        </div>
      </header>

      <div
        className="feed-scroll"
        data-zoom-pane="feed"
        ref={scrollRef}
        onTouchStart={(e) => {
          touchYRef.current = e.touches[0]?.clientY ?? null;
        }}
        onTouchMove={(e) => {
          markUserScrolling();
          const previousY = touchYRef.current;
          const nextY = e.touches[0]?.clientY;
          if (previousY != null && nextY != null && nextY > previousY) {
            leavePresent();
          } else if (previousY != null && nextY != null && nextY < previousY) {
            manualAwayRef.current = false;
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
          if (e.key === "ArrowUp" || e.key === "PageUp" || e.key === "Home") {
            markUserScrolling();
            leavePresent();
          } else if (
            e.key === "ArrowDown" ||
            e.key === "PageDown" ||
            e.key === "End" ||
            e.key === " "
          ) {
            markUserScrolling();
            manualAwayRef.current = false;
          }
        }}
        onScroll={(e) => {
          if (!loadedFeedId) return;
          const el = e.currentTarget;
          const previousTop = scrollTopRef.current;
          const nextTop = el.scrollTop;
          scrollTopRef.current = nextTop;
          if (manualAwayRef.current) markUserScrolling();
          if (manualAwayRef.current && nextTop < el.clientHeight * 2.5) {
            prependOlderFeed(el);
          }
          // Browser-generated scroll events also fire when streaming content,
          // the composer, or the mobile viewport changes size. Those must not
          // cancel live-follow. Explicit wheel/touch/key gestures above turn it
          // off; reaching the real end here is the only implicit state change.
          if (nextTop < previousTop - BOTTOM_STICK_EPSILON) {
            // Covers scrollbar drags and upward wheel/touch scrolling even when
            // streaming changed the feed geometry in the same frame. A decrease
            // is not by itself reader intent, though: the browser produces one
            // by clamping whenever the end moves up under someone sitting on it
            // (see CLAMP_SLACK). Only a decrease that leaves the reader off the
            // end is a gesture.
            //
            // This is the one place that reads layout inside a scroll handler,
            // and it is deliberately on this branch only: content growing under
            // a follower never gets here, so the streaming path stays free of
            // forced reflows.
            const end = Math.max(0, el.scrollHeight - el.clientHeight);
            if (end - nextTop > CLAMP_SLACK) leavePresent();
          }
          // Reading scrollHeight/clientHeight here can force layout while the
          // browser is trying to paint the current wheel frame. Coalesce the
          // geometry read to one rAF so a burst of trackpad events does not
          // repeatedly flush layout.
          if (scrollMetricsRafRef.current === null) {
            scrollMetricsRafRef.current = window.requestAnimationFrame(() => {
              scrollMetricsRafRef.current = null;
              const current = scrollRef.current;
              if (!current || !loadedFeedId) return;
              const scrollHeight = current.scrollHeight;
              const clientHeight = current.clientHeight;
              scrollHeightRef.current = scrollHeight;
              clientHeightRef.current = clientHeight;
              const distanceFromEnd = Math.max(
                0,
                scrollHeight - current.scrollTop - clientHeight,
              );
              if (
                !manualAwayRef.current &&
                distanceFromEnd <= BOTTOM_STICK_EPSILON
              ) {
                stickRef.current = true;
              }
              const present = distanceFromEnd < BOTTOM_SLACK;
              setAtPresent((prev) => (prev === present ? prev : present));
            });
          }
        }}
      >
        {findPresent && (
          <div className={`chat-find-slot${findClosing ? " closing" : ""}`}>
            <ChatFindBar
              inputRef={findInputRef}
              query={findQuery}
              onQueryChange={(value) => {
                setFindQuery(value);
                setFindIndex(0);
              }}
              matchCount={findMatches.length}
              matchIndex={activeFindIndex}
              onPrevious={() => {
                if (findMatches.length === 0) return;
                setFindIndex((index) => (index - 1 + findMatches.length) % findMatches.length);
              }}
              onNext={() => {
                if (findMatches.length === 0) return;
                setFindIndex((index) => (index + 1) % findMatches.length);
              }}
              onClose={closeFind}
            />
          </div>
        )}
        <div className="feed-inner" ref={feedInnerRef}>
          {/* Phones only — the desktop header carries this instead. It rides in
              the scrollport so it greets you on arrival and then scrolls away,
              costing nothing at rest under the Dynamic Island. Suppressed while
              the new-thread empty state is up: that state states the folder
              itself, in a sentence, and two copies read as a mistake. */}
          {project?.path && !hermesHome && !quickHome && !(feedLen === 0 && projectDraft) && (
            <div
              className="feed-project-caption"
              title={project.path}
              aria-label={`Project directory: ${project.path}`}
            >
              {elidePathMiddle(project.path)}
            </div>
          )}
          {state.feedLoading && <div className="feed-note note-status">loading log…</div>}
          {!state.feedLoading && feedLen === 0 && (
            <div className="feed-empty">
              {projectDraft ? (
                <WorkOnProject
                  project={project}
                  agent={draft.agent}
                  settings={draft.settings}
                />
              ) : (
                <p>
                  {draft
                    ? hermesHome
                      ? `Say hello to ${hermesGatewayName ?? "your Hermes agent"}`
                      : quickHome
                        ? "Ask anything…"
                        : "Let's get started…"
                    : "No traffic on this channel yet."}
                </p>
              )}
            </div>
          )}
          {state.feed.slice(feedStart).map((item) => {
            const matched = findMatchIds.has(item.id);
            const current = activeFindId === item.id;
            return (
              <FeedFindItem
                key={item.id}
                item={item}
                matched={matched}
                current={current}
                itemRef={findItemRef(item.id)}
                render={feedRender}
                thoughtTime={thoughtTimes[item.id]}
              />
            );
          })}
          {busy && (
            <div className="working-row">
              <span className="working-signal" aria-hidden="true">
                <i /><i /><i />
              </span>
              <span className="working-copy">
                <strong>
                  {thread!.status === "waiting_approval"
                    ? "Holding for approval"
                    : runningSubagents > 0
                      ? "Waiting on a child agent"
                      : "Working"}
                </strong>
                <em>{runningSubagents > 0 ? "still in motion…" : "following the thread…"}</em>
              </span>
              <span className="working-sweep" aria-hidden="true" />
            </div>
          )}
        </div>
      </div>

      {/* Pinned above the composer (outside the scroll container) so it stays
          put on every layout — desktop, the stacked mobile panes, and zoom. */}
      <div className="composer-dock" ref={composerDockRef}>
        {feedLen > 0 && !atPresent && (
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
        <Composer
          thread={thread ?? null}
          quickMode={quickHome ? quickMode : undefined}
          replyTo={replyTo}
          onClearReply={() => setReplyTo(null)}
        />
      </div>
    </section>
  );
}
