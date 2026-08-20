import {
  Fragment,
  useCallback,
  useEffect,
  useLayoutEffect,
  memo,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  getNotifyPrefs,
  isWorkspaceSubscribed,
  subscribeNotifyPrefs,
  toggleWorkspaceNotify,
} from "../lib/notify";
import {
  getSidebarPrefs,
  SIDEBARPREFS_EVENT,
  type ProjectLayout,
  type SidebarPrefs,
} from "../lib/appearance";
import { createPortal } from "react-dom";
import type { Project, Thread, Workspace } from "../lib/protocol";
import {
  HERMES_HOME_PROJECT_ID,
  isQuickHomeProjectId,
  OWNER_PERSON_ID,
} from "../lib/protocol";
import { showHermesAgents } from "../lib/agentVisibility";
import { hermesActive, hermesDormant, hermesGatewayId } from "../lib/hermesBinding";
import {
  getSidebarView,
  setSidebarView,
  subscribeSidebarView,
  type SidebarView,
} from "../lib/sidebarView";
import { PORTRAITS_EVENT, resolvePortrait } from "../lib/portraits";
import { timeAgo } from "../lib/format";
import {
  findThread,
  hermesAttentionThreads,
  allProjects,
  allWorkspaces,
  personById,
  projectActivity,
  workspaceServer,
  threadNeedsAttention,
  threadInView,
  threadSettled,
  useStore,
  type AppState,
  type ProjectActivity,
} from "../state/store";
import { SettingsScreen } from "./SettingsPopover";
import { PeopleRow, PersonAvatar } from "./PeopleRow";
import { CrestBadge } from "./legacy/Crest";
import { UsageMeter } from "./UsageMeter";
import { VersionBadge } from "./VersionBadge";
import { openProjectWindow } from "../lib/solo";
import { pickAvatarImage, pickSidebarImage } from "../lib/sidebarImage";
import { MachineAvatar, machineLook } from "./MachineAvatar";
import { useAvatarHoverPreview } from "./AvatarHoverPreview";
import { HermesPresenceDot, hermesPresence } from "./HermesPresence";
import {
  useHoverCard,
  ThreadHoverCardBody,
  WorkspaceHoverCardBody,
  usePeekThreadPreview,
} from "./ThreadHoverCard";
import { ContextMenu } from "./ContextMenu";
import { DirPicker } from "./DirPicker";
import { useLongPressMenu, type MenuPoint } from "../lib/longPress";
import { applyProjectOrder, applyThreadOrder, mergeProjectOrder } from "../lib/projectOrder";
import { useReorderDrag, type ReorderHandleProps } from "../lib/reorder";
import {
  useSidebarLayout,
  isNonDefaultLayout,
  SIDEBAR_WIDTH_DEFAULT,
  type ChatFolder,
  type SidebarLayout,
} from "../lib/sidebarLayout";
import {
  AgentMark,
  ArchiveIcon,
  BellIcon,
  CheckIcon,
  ChevronIcon,
  ClockIcon,
  CompassIcon,
  EyeIcon,
  FilterIcon,
  FolderClosedIcon,
  FolderIcon,
  FolderPlusIcon,
  GearIcon,
  LoaderIcon,
  PanelLeftIcon,
  GripIcon,
  MoreIcon,
  NotebookPenIcon,
  PencilIcon,
  PlusIcon,
  PopoutIcon,
  SearchIcon,
  StarIcon,
  TrashIcon,
  UndoIcon,
  XIcon,
} from "./icons";
import wordmarkUrl from "../assets/threadknot-wordmark.png";

const THREAD_PAGE_SIZE = 5;
/** Layouts that show one project at a time hand it the whole sidebar, so the
 *  5-row page that suits a stack of nine sections just leaves empty space
 *  above a "load more". */
const SOLO_PROJECT_PAGE_SIZE = 20;
/** The shelf pages in bigger bites than the live list: it is history, and
 *  you are usually scanning it rather than reading it. */
const SETTLED_PAGE_SIZE = 15;
const NO_CONTENT_MATCHES: ReadonlySet<string> = new Set();

function threadMatches(
  thread: Thread,
  filter: string,
  contentMatches: ReadonlySet<string>,
): boolean {
  return (
    (thread.title || "untitled").toLowerCase().includes(filter) ||
    contentMatches.has(thread.id)
  );
}

/** Descending string compare that returns a real 0 on ties. A comparator
 *  that never returns 0 is inconsistent, and V8 is then free to order equal
 *  rows differently between renders — exactly the shuffling the static sort
 *  exists to prevent. */
function cmpDesc(a: string, b: string): number {
  return a === b ? 0 : a < b ? 1 : -1;
}

/** Split one section's chats into what is in play and what is parked, each
 *  in a stable order. Shared by workspace sections and Hermes groups so the
 *  rule cannot drift between them.
 *
 *  Depends on `attention`/`activeThreadId` rather than the whole store: the
 *  store gets a fresh identity on every streamed token, which would re-run
 *  this partition and both sorts dozens of times a second during a live
 *  turn — in every section at once. */
/** Page the active list WITHOUT ever hiding a chat that wants a human.
 *
 *  The old list was ordered by `updatedAt`, which made the paging cut safe by
 *  accident: whatever had just spoken was at index 0. A static order severs
 *  that link — a chat created months ago can be blocked on an approval and
 *  sit at index 30 — so the cut has to be made safe on purpose. Rows keep
 *  their static position; the ones pulled past the cut are simply exempt
 *  from it. */
/** How long a chat you just pulled out of the shelf is exempt from the page
 *  cut. Un-settling sorts it back into a STATIC order, so an old chat can land
 *  well past "load more" and read as having vanished — the one thing the
 *  gesture must not do. After the window it is an ordinary active chat. */
const KEPT_ACTIVE_GRACE_MS = 30 * 60 * 1000;

export function isRecentlyKeptActive(thread: Thread, now: number): boolean {
  if (!thread.keptActiveAt) return false;
  const at = Date.parse(thread.keptActiveAt);
  return Number.isFinite(at) && at > now - KEPT_ACTIVE_GRACE_MS;
}

function pageActiveThreads(
  active: Thread[],
  limit: number,
  mustShow: (t: Thread) => boolean,
): Thread[] {
  if (active.length <= limit) return active;
  const head = new Set(active.slice(0, limit).map((t) => t.id));
  return active.filter((t) => head.has(t.id) || mustShow(t));
}

/** What the section header counts.
 *
 *  Open, it counts what is in play — the shelf line right below carries its
 *  own number. Collapsed, there is no shelf line to explain a zero, and
 *  auto-settle ships ON: the first launch after a long weekend would show a
 *  fleet of headers all reading 0 with no threads anywhere, which reads as
 *  "the update ate my data". So a collapsed section with parked chats shows
 *  both. While searching, every match counts, shelf included. */
function countLabel(
  activeCount: number,
  settledCount: number,
  forceOpen: boolean,
  open: boolean,
): string {
  if (forceOpen) return String(activeCount + settledCount);
  if (open || settledCount === 0) return String(activeCount);
  return `${activeCount} · ${activeCount + settledCount}`;
}

/** Pull dispatched workers out of a flat thread list and file them under the
 *  thread that sent them.
 *
 *  A fan-out to three machines creates three threads, and left flat they sort
 *  to the top by recency — so asking for a cross-platform build buries the
 *  conversation you asked from underneath its own workers. They belong *under*
 *  it: that is the actual relationship, and it is the one thing Threadknot has
 *  that a subagent API does not — the work is inspectable, but only if you can
 *  find it.
 *
 *  A worker whose parent is NOT in this list stays top-level. That happens when
 *  the parent is filtered out by a search, paged away, or lives in a section
 *  the worker does not — and a thread that renders nowhere is worse than one
 *  rendered at the wrong depth. Depth is one by construction
 *  (`MAX_DISPATCH_DEPTH`), so this never needs to recurse. */
function groupDispatchChildren(threads: Thread[]): {
  parents: Thread[];
  workersOf: Map<string, Thread[]>;
} {
  const present = new Set(threads.map((t) => t.id));
  const workersOf = new Map<string, Thread[]>();
  const parents: Thread[] = [];
  for (const t of threads) {
    const parentId = t.dispatch?.parentThreadId;
    if (parentId && parentId !== t.id && present.has(parentId)) {
      const list = workersOf.get(parentId);
      if (list) list.push(t);
      else workersOf.set(parentId, [t]);
    } else {
      parents.push(t);
    }
  }
  // Oldest first, so one fan-out reads in the order it was sent rather than
  // reshuffling itself every time a worker says something.
  for (const list of workersOf.values()) {
    list.sort((a, b) => (a.createdAt < b.createdAt ? -1 : 1));
  }
  return { parents, workersOf };
}

function useSettledSplit(
  threads: Thread[],
  autoSettleDays: number | null,
  now: number,
): { active: Thread[]; settled: Thread[] } {
  const { state } = useStore();
  const { attention, activeThreadId } = state;
  return useMemo(() => {
    const active: Thread[] = [];
    const settled: Thread[] = [];
    const ctx = { attention, activeThreadId };
    for (const t of threads) {
      (threadSettled(ctx, t, autoSettleDays, now) ? settled : active).push(t);
    }
    active.sort(
      (a, b) => cmpDesc(a.createdAt, b.createdAt) || cmpDesc(a.id, b.id),
    );
    settled.sort(
      (a, b) =>
        cmpDesc(a.settledAt ?? a.updatedAt, b.settledAt ?? b.updatedAt) ||
        cmpDesc(a.id, b.id),
    );
    return { active, settled };
  }, [threads, attention, activeThreadId, autoSettleDays, now]);
}

/** The workers a thread has dispatched, as a collapsible tail under its row.
 *
 *  Collapsed by default: the point of nesting them is that asking for a build
 *  on three machines should cost one line of sidebar, not four. Left alone it
 *  opens itself when one of the workers is the thread you are looking at or
 *  wants attention, because a group that hides the row you selected is worse
 *  than no grouping at all.
 *
 *  But that rule may only ever *suggest*. It used to be OR'd over the toggle,
 *  which meant the exact group you most wanted to fold away — the busy one —
 *  was the one whose chevron did nothing: `setOpen(false)` while a worker was
 *  active recomputed straight back to open, so the group appeared to ignore
 *  the tap. A deliberate collapse now outranks the auto-open and survives
 *  later attention; what the group would have opened for it says on its own
 *  line instead (a dot for attention, a tint for "you are in here"), so
 *  nothing is silently swallowed by the fold.
 *
 *  A flat sibling list rather than a wrapping container, like `SettledShelf`:
 *  `ThreadRow` returns a fragment whose menu portal is a deliberate sibling of
 *  the row, and wrapping that changes where its clicks land. */
function DispatchWorkers({
  workers,
  forceOpen,
  view,
}: {
  workers: Thread[];
  forceOpen: boolean;
  view: SidebarLayout["view"];
}) {
  const { state } = useStore();
  // null = never touched, so follow the auto rule. A boolean is a decision the
  // user made, and it outranks it.
  const [choice, setChoice] = useState<boolean | null>(null);
  if (workers.length === 0) return null;
  const holdsActive = workers.some((w) => state.activeThreadId === w.id);
  const wants = workers.some((w) => threadNeedsAttention(state, w));
  const showing = forceOpen || (choice ?? (holdsActive || wants));
  // "3 running" is the number worth reading at a glance; the total is only
  // interesting once they have all stopped.
  const live = workers.filter((w) => w.status !== "idle").length;
  const label = workers.length === 1 ? "1 worker" : `${workers.length} workers`;
  return (
    <>
      <button
        type="button"
        className={`dispatch-shelf${showing ? " open" : ""}${
          !showing && holdsActive ? " holds-active" : ""
        }`}
        aria-expanded={showing}
        aria-label={`${showing ? "Hide" : "Show"} ${label}`}
        // Toggle against what is actually on screen, not against a private
        // flag: with the auto-open above it, `!choice` can be a no-op.
        onClick={() => setChoice(!showing)}
      >
        <ChevronIcon size={11} open={showing} className="row-chevron" />
        <span className="dispatch-shelf-label">{label}</span>
        {!showing && wants && (
          <span className="dispatch-shelf-dot" aria-hidden="true" />
        )}
        {live > 0 && <span className="dispatch-shelf-live">{live} running</span>}
      </button>
      {showing &&
        workers.map((w) => (
          <ThreadRow
            key={w.id}
            thread={w}
            active={state.activeThreadId === w.id}
            view={view}
            nested
          />
        ))}
    </>
  );
}

/** The parked tail of a section: one collapsed line by default, so history
 *  costs a row of sidebar instead of N. While a search is running the whole
 *  shelf is flattened open — a chat you cannot find is worse than a chat you
 *  have to un-settle. */
function SettledShelf({
  threads,
  forceOpen,
  inHermesView = false,
  view,
  folders = [],
  folderAssignments = {},
  onMoveToFolder,
  onAddFolder,
}: {
  threads: Thread[];
  forceOpen: boolean;
  /** Set when this shelf hangs off a Hermes gateway group; forwarded so a
   *  parked chat a local agent has taken back still greys correctly. */
  inHermesView?: boolean;
  /** Sidebar presentation, forwarded to each shelf row. */
  view: SidebarLayout["view"];
  folders?: ChatFolder[];
  folderAssignments?: Record<string, string>;
  onMoveToFolder?: (threadId: string, folderId?: string) => void;
  onAddFolder?: (threadId: string) => void;
}) {
  const { state } = useStore();
  const [open, setOpen] = useState(false);
  const [count, setCount] = useState(SETTLED_PAGE_SIZE);
  if (threads.length === 0) return null;
  const shown = forceOpen ? threads : open ? threads.slice(0, count) : [];
  const hidden = threads.length - shown.length;
  return (
    <>
      {!forceOpen && (
        <button
          type="button"
          className={`settled-shelf${open ? " open" : ""}`}
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <ChevronIcon size={11} open={open} className="row-chevron" />
          <FolderIcon size={13} className="settled-shelf-icon" />
          <span className="settled-shelf-label">settled</span>
          <span className="settled-shelf-count">{threads.length}</span>
        </button>
      )}
      {shown.map((t) => (
        <ThreadRow
          key={t.id}
          thread={t}
          active={state.activeThreadId === t.id}
          inHermesView={inHermesView}
          view={view}
          settled
          lit={forceOpen}
          folders={folders}
          folderId={folderAssignments[t.id]}
          onMoveToFolder={onMoveToFolder}
          onAddFolder={onAddFolder}
        />
      ))}
      {open && hidden > 0 && !forceOpen && (
        <button
          className="load-more-threads"
          onClick={() => setCount((c) => c + SETTLED_PAGE_SIZE)}
        >
          <span>load more</span>
          <span className="load-more-count">
            {Math.min(SETTLED_PAGE_SIZE, hidden)} of {hidden}
          </span>
        </button>
      )}
    </>
  );
}

/** The folderless conversation home. It borrows the thread rows and settled
 * shelf from workspaces, but is deliberately a destination rather than a fake
 * workspace: no folder avatar, roots menu, pop-out, or project actions. */
function QuickChatsSection({
  threads,
  forceOpen,
  autoSettleDays,
  now,
  view,
}: {
  threads: Thread[];
  forceOpen: boolean;
  autoSettleDays: number | null;
  now: number;
  view: SidebarLayout["view"];
}) {
  const { state, actions } = useStore();
  const [visibleThreadCount, setVisibleThreadCount] = useState(SOLO_PROJECT_PAGE_SIZE);
  // Workers are filed under the thread that sent them, so the paging, the
  // settled split and the header count below all see one row per CHAT rather
  // than one per agent a chat happens to have out.
  const { parents, workersOf } = useMemo(
    () => groupDispatchChildren(threads),
    [threads],
  );
  const { active, settled } = useSettledSplit(parents, autoSettleDays, now);
  const shown = forceOpen
    ? active
    : pageActiveThreads(
        active,
        visibleThreadCount,
        (thread) =>
          threadNeedsAttention(state, thread) ||
          state.activeThreadId === thread.id ||
          isRecentlyKeptActive(thread, now),
      );
  const remaining = active.length - shown.length;
  const activity = projectActivity(state, threads);

  return (
    <section className="quick-chats" aria-label="Quick threads">
      <div className="quick-chats-label">Chats</div>
      {shown.length === 0 && settled.length === 0 && forceOpen ? (
        <div className="sidebar-empty quick-search-empty">
          <p>No matching quick threads.</p>
        </div>
      ) : shown.length === 0 && settled.length === 0 ? (
        <button
          type="button"
          className="quick-chats-empty"
          onClick={() => actions.openQuickDraft()}
        >
          <span>Ask an odd question, compare a file, or diagnose this computer.</span>
          <strong>Start a quick thread</strong>
        </button>
      ) : (
        <div className="project-threads quick-chat-threads">
          {shown.map((thread) => (
            <Fragment key={thread.id}>
              <ThreadRow
                thread={thread}
                active={state.activeThreadId === thread.id}
                view={view}
              />
              <DispatchWorkers
                workers={workersOf.get(thread.id) ?? []}
                forceOpen={forceOpen}
                view={view}
              />
            </Fragment>
          ))}
          {remaining > 0 && (
            <button
              className="load-more-threads"
              onClick={() =>
                setVisibleThreadCount((count) => count + SOLO_PROJECT_PAGE_SIZE)
              }
            >
              <span>load more</span>
              <span className="load-more-count">
                {Math.min(SOLO_PROJECT_PAGE_SIZE, remaining)} of {remaining}
              </span>
            </button>
          )}
          <SettledShelf threads={settled} forceOpen={forceOpen} view={view} />
        </div>
      )}
    </section>
  );
}

/** The project rail: a compact project index down the left edge. The selected
 *  project fills the rest of the sidebar, while every other project remains
 *  visible so unread work is never hidden behind a picker. */
/** The one project-level status light, shared by every layout so "working"
 *  and "needs you" never mean different things in different modes.
 *
 *  Motion carries the meaning, but never ALONE: working is a teal orbiting
 *  ring, attention a larger amber dot with a halo. Under
 *  prefers-reduced-motion the animations stop and the two stay
 *  distinguishable by colour and size. */
function ProjectPulse({
  activity,
  count,
}: {
  activity: ProjectActivity;
  /** Unread chats, for the tooltip only. */
  count?: number;
}) {
  if (activity === null) return null;
  const label =
    activity === "attention"
      ? count && count > 0
        ? `${count} chat${count === 1 ? "" : "s"} need you`
        : "needs you"
      : "working";
  return (
    <span
      className={`project-pulse pulse-${activity}`}
      role="status"
      aria-label={label}
      title={label}
    />
  );
}

/**
 * Which chat switching to a project should drop you into, in the order you
 * would pick one yourself:
 *
 *  1. the one asking for you — a pending approval or an unread finish;
 *  2. else whichever is mid-turn, so you land on the work in progress;
 *  3. else the most recent chat that is still live;
 *  4. else the most recent one at all, shelf included — a project whose chats
 *     have all settled should still show you its last chat rather than
 *     stranding you in the project you just left.
 *
 * `threads` arrives newest-first, so "first match" is also "most recent" in
 * every branch. Null only when the project has no chats at all.
 */
function landingThread(
  state: AppState,
  threads: readonly Thread[],
  autoSettleDays: number | null,
  now: number,
): Thread | null {
  if (threads.length === 0) return null;
  return (
    threads.find((t) => threadNeedsAttention(state, t)) ??
    threads.find((t) => t.status === "running") ??
    threads.find((t) => !threadSettled(state, t, autoSettleDays, now)) ??
    threads[0]
  );
}

/** A stable accent per project. The rail has no room for names, so the badge
 *  IS the identity — and every project on one machine would otherwise draw
 *  the same machine initials in the same color and be indistinguishable.
 *  Hue from a hash of the id; fixed saturation/lightness so no project shouts
 *  louder than another or fights the brass selection ring. */
function projectAccent(id: string): string {
  let h = 0;
  for (let i = 0; i < id.length; i += 1) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return `hsl(${h % 360} 52% 60%)`;
}

function ProjectRail({
  quickThreads,
  quickOn,
  workspaces,
  hidden,
  threadsByWorkspace,
  shownId,
  onPickQuick,
  onPick,
  onMenu,
  onStash,
  onReorder,
  reorderEnabled,
}: {
  quickThreads: Thread[];
  quickOn: boolean;
  workspaces: Workspace[];
  /** Every stashed workspace, whether or not the view toggle is currently
   *  showing them inline — the stash button is how you get them back, so it has
   *  to know about them even when nothing above it does. */
  hidden: Workspace[];
  threadsByWorkspace: Map<string, Thread[]>;
  shownId: string | null;
  onPickQuick: () => void;
  onPick: (id: string) => void;
  /** Right-click on a tile: the same workspace menu the section headers and
   *  the picker bar open, for the project under the cursor (not necessarily
   *  the one on screen). No long-press twin here — on touch the rail already
   *  spends its hold on drag-to-reorder, and the bar below opens the same menu
   *  for the shown project. */
  onMenu: (point: MenuPoint, workspace: Workspace) => void;
  /** The stash button was pressed — the Sidebar owns the menu listing what is
   *  in there, alongside every other portaled menu. */
  onStash: (point: MenuPoint) => void;
  /** New top-to-bottom order after a drag. */
  onReorder: (ids: string[]) => void;
  reorderEnabled: boolean;
}) {
  const { state } = useStore();
  const railRef = useRef<HTMLElement | null>(null);
  // The rail is its own scroller, so it is also what a drag near the edge
  // scrolls.
  const drag = useReorderDrag({
    containerRef: railRef,
    onCommit: onReorder,
    enabled: reorderEnabled,
  });
  // The loudest thing happening behind the stash button, so what is in there
  // can still ask for you. Same precedence as a tile: needing you outranks
  // merely working.
  const stashActivity: ProjectActivity = hidden.reduce<ProjectActivity>(
    (worst, w) => {
      if (worst === "attention") return worst;
      const a = projectActivity(state, threadsByWorkspace.get(w.id) ?? []);
      return a === "attention" ? a : (worst ?? a);
    },
    null,
  );
  const quickActivity = projectActivity(state, quickThreads);
  return (
    <nav className="project-rail" aria-label="Destinations" ref={railRef}>
      <div className="rail-home">
        <button
          type="button"
          className={`rail-item rail-quick${quickOn ? " on" : ""}${
            quickActivity ? ` rail-${quickActivity}` : ""
          }`}
          aria-current={quickOn ? "true" : undefined}
          aria-label={`Quick threads${
            quickActivity === "attention"
              ? " — needs you"
              : quickActivity === "working"
                ? " — working"
                : ""
          }`}
          title="Quick threads"
          onClick={onPickQuick}
        >
          <span className="rail-pip" aria-hidden />
          <span className="rail-quick-face" aria-hidden>
            <PlusIcon size={21} />
          </span>
          <span
            className={`rail-ring${quickActivity === "working" ? " underway" : ""}`}
            aria-hidden
          />
          <span
            className={`rail-dot${quickActivity === "attention" ? " lit" : ""}`}
            aria-hidden
          />
        </button>
        <span className="rail-divider" aria-hidden />
      </div>
      {workspaces.map((w) => {
        const threads = threadsByWorkspace.get(w.id) ?? [];
        const activity = projectActivity(state, threads);
        const on = w.id === shownId;
        return (
          <button
            key={w.id}
            type="button"
            data-reorder-id={w.id}
            data-drop={drag.dropId === w.id ? drag.dropSide : undefined}
            className={`rail-item${on ? " on" : ""}${
              activity ? ` rail-${activity}` : ""
            }${drag.draggingId === w.id ? " dragging" : ""}${
              w.hidden ? " hidden-ws" : ""
            }`}
            {...drag.handleProps(w.id)}
            aria-current={on ? "true" : undefined}
            aria-label={`${w.name}${w.hidden ? " — hidden" : ""}${
              activity === "attention"
                ? " — needs you"
                : activity === "working"
                  ? " — working"
                  : ""
            }`}
            title={w.hidden ? `${w.name} (hidden)` : w.name}
            aria-haspopup="menu"
            onClick={() => onPick(w.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              // Deliberately does NOT switch project: a right-click asks about
              // the tile, it doesn't navigate to it.
              onMenu({ x: e.clientX, y: e.clientY }, w);
            }}
          >
            {/* The edge pill: grows on hover, full height when open. */}
            <span className="rail-pip" aria-hidden />
            {/* The project marker is intentionally quiet in the rail. The
                native title and accessible label carry the full name; the
                rail should not turn a navigation marker into a profile card. */}
            <MachineAvatar
              image={w.image}
              color={projectAccent(w.id)}
              name={w.name}
              size={34}
              preview={false}
            />
            {/* The ring rides OUTSIDE the avatar so an image-backed project
                shows it as clearly as an initials one.

                Both indicators stay MOUNTED and switch on a class rather than
                entering and leaving the tree. The ring is masked, filtered and
                animates a registered custom property, so WebKitGTK gives it its
                own render surface — and inserting or removing one of those
                rebuilds the layer tree, which the webview pays for with a
                full-window repaint. That landed on exactly the frame a turn
                started or finished somewhere in the fleet: the screen-wide
                flicker when another chat completed. */}
            <span
              className={`rail-ring${activity === "working" ? " underway" : ""}`}
              aria-hidden
            />
            <span
              className={`rail-dot${activity === "attention" ? " lit" : ""}`}
              aria-hidden
            />
          </button>
        );
      })}
      {/* The way back. Only mounted once something is actually stashed, so an
          empty rail stays empty — and it sits BELOW the tiles rather than in
          the view popover because that is where the projects it holds used to
          be, and the popover is three taps away on a phone.

          It keeps its own status light: a project you put away is still running
          agents, and going silent about a turn that finished (or an approval
          waiting) is how "hidden" would turn into "lost". */}
      {hidden.length > 0 && (
        <button
          type="button"
          className={`rail-item rail-stash${
            stashActivity ? ` rail-${stashActivity}` : ""
          }`}
          aria-haspopup="menu"
          aria-label={`${hidden.length} hidden project${
            hidden.length === 1 ? "" : "s"
          }${stashActivity === "attention" ? " — one needs you" : ""}`}
          title={`${hidden.length} hidden project${
            hidden.length === 1 ? "" : "s"
          }`}
          // Anchored to the tile, not the pointer, like the picker bar's menu:
          // the list flies out beside the button it came from, and a keyboard
          // activation (clientX/clientY of 0) doesn't drop it in the corner.
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            onStash({ x: r.right + 6, y: r.top });
          }}
        >
          <span className="rail-pip" aria-hidden />
          <span className="rail-stash-face" aria-hidden>
            <EyeIcon size={15} off />
            <span className="rail-stash-count">{hidden.length}</span>
          </span>
          <span
            className={`rail-dot${stashActivity === "attention" ? " lit" : ""}`}
            aria-hidden
          />
        </button>
      )}
    </nav>
  );
}

/** Inline renamer for the picker/rail bar. Those two layouts hide the section
 *  header the other layouts put this input in, so "Rename workspace" from
 *  their menus had nowhere to render and appeared to do nothing. Mounted only
 *  while renaming, so the draft starts from the current name and needs no
 *  syncing effect. */
function PickerRenameInput({
  workspace,
  onDone,
}: {
  workspace: Workspace;
  onDone: () => void;
}) {
  const { actions } = useStore();
  const [name, setName] = useState(workspace.name);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);

  function commit() {
    const next = name.trim();
    onDone();
    if (next && next !== workspace.name) {
      void actions.renameWorkspace(workspace.id, next);
    }
  }

  return (
    <input
      ref={ref}
      className="thread-rename-input workspace-rename-input"
      value={name}
      aria-label="Rename workspace"
      onChange={(e) => setName(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onBlur={commit}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") commit();
        else if (e.key === "Escape") onDone();
      }}
    />
  );
}

/** Small, focused naming step opened from the workspace switcher. Folder
 * creation is organizational only, so it belongs beside that switcher rather
 * than in the filesystem-oriented project picker. */
function ChatFolderDialog({
  folders,
  onClose,
  onCreate,
}: {
  folders: ChatFolder[];
  onClose: () => void;
  onCreate: (name: string) => void;
}) {
  const [name, setName] = useState("");
  const [error, setError] = useState("");

  function submit() {
    const next = name.trim();
    if (!next) {
      setError("Enter a folder name.");
      return;
    }
    if (folders.some((folder) => folder.name.toLowerCase() === next.toLowerCase())) {
      setError("A folder with that name already exists.");
      return;
    }
    onCreate(next);
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal chat-folder-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="chat-folder-title"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <span id="chat-folder-title">Add chat folder</span>
          <button type="button" className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>
        <label className="chat-folder-field">
          <span>Folder name</span>
          <input
            className="modal-input"
            autoFocus
            value={name}
            placeholder="e.g. Client work"
            onChange={(event) => {
              setName(event.target.value);
              setError("");
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") submit();
              else if (event.key === "Escape") onClose();
            }}
          />
        </label>
        {error && <div className="modal-error">{error}</div>}
        <div className="modal-actions">
          <button type="button" className="settings-toggle" onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="settings-toggle on" onClick={submit}>
            Add folder
          </button>
        </div>
      </div>
    </div>
  );
}

function chooseSidebarImage(save: (image: string) => Promise<unknown>) {
  void pickSidebarImage()
    .then((image) => (image ? save(image) : undefined))
    .catch((error: unknown) => {
      window.alert(error instanceof Error ? error.message : String(error));
    });
}

/** One workspace root resolved for display: which machine, which folder,
 *  whether we can reach it right now. */
interface MemberView {
  machineId: string;
  projectId: string;
  rootLabel: string;
  machineLabel: string;
  isLocal: boolean;
  online: boolean;
  /** Folder path snapshot, for the workspace hover card's per-root lines. */
  path: string;
}

/** Two-step destructive button: first click arms, second confirms. */
export function DangerButton({
  label,
  onConfirm,
}: {
  label: string;
  onConfirm: () => void;
}) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 2600);
    return () => clearTimeout(t);
  }, [armed]);
  return (
    <button
      className={`icon-btn danger-btn${armed ? " armed" : ""}`}
      aria-label={label}
      title={armed ? "Click again to confirm" : label}
      onClick={(e) => {
        e.stopPropagation();
        if (armed) onConfirm();
        else setArmed(true);
      }}
    >
      {armed ? (
        <span className="danger-confirm">sure?</span>
      ) : (
        <TrashIcon size={13} />
      )}
    </button>
  );
}

function ThreadRow({
  thread,
  active,
  settled = false,
  lit = false,
  nested = false,
  inHermesView = false,
  view,
  folders = [],
  folderId,
  onMoveToFolder,
  onAddFolder,
  reorder,
}: {
  thread: Thread;
  active: boolean;
  /** Rendered as a compact row inside the settled shelf. The row still opens
   *  the chat normally — settling is about attention, not access. */
  settled?: boolean;
  /** Keep a shelf row at full strength. Set while searching: a match that
   *  renders at resting-shelf opacity reads as a disabled non-result. */
  lit?: boolean;
  /** A dispatched worker, rendered indented under the thread that sent it.
   *  Presentation only — it is a real thread and opens like any other. */
  nested?: boolean;
  /** Rendered inside a Hermes gateway's group rather than a workspace. The
   *  gateway is already named by the group header, so the row drops its
   *  gateway chip; in exchange it says which local agent has the chat when
   *  Hermes is not the one holding the next turn. */
  inHermesView?: boolean;
  /** Which sidebar presentation to render. Threaded down from the layout hook
   *  (a fresh useSidebarLayout() here would be a second, out-of-sync copy). */
  view: SidebarLayout["view"];
  /** Chat folders belonging to this thread's workspace. Omitted for Quick
   *  Threads, Hermes chats, and dispatched workers. */
  folders?: ChatFolder[];
  folderId?: string;
  onMoveToFolder?: (threadId: string, folderId?: string) => void;
  onAddFolder?: (threadId: string) => void;
  /** Drag-to-reorder wiring from the list this row belongs to. Absent on rows
   *  whose list has no manual order: the settled shelf, dispatched workers,
   *  Quick Threads and the Hermes view. */
  reorder?: {
    handle: ReorderHandleProps;
    dragging: boolean;
    drop?: "before" | "after";
  };
}) {
  const { state, dispatch, actions } = useStore();
  // One modifier appended to every variant's className, the same way `.slim`
  // and `.recede` are done — the row has four return paths and a wrapper div
  // would change where its menu portal's clicks land.
  const nest = nested ? " is-worker" : "";
  const needsAttention = !active && threadNeedsAttention(state, thread);
  // Inverted prominence: a chat that is merely BUSY is not your problem yet,
  // so it recedes. Brightness is reserved for rows that want a human —
  // pending approvals and unread finishes — which is what makes a glance at
  // a 5-project sidebar answer "what needs me?" instead of "what exists?".
  const recede = !active && !needsAttention && thread.status === "running";
  const isGenerating = thread.status === "running";
  // Keep the loader mounted for one short beat after a turn ends so the
  // spinner can scale back down instead of disappearing mid-motion.
  const [showGeneratingIndicator, setShowGeneratingIndicator] =
    useState(isGenerating);
  const wasGenerating = useRef(isGenerating);
  useEffect(() => {
    if (isGenerating) {
      wasGenerating.current = true;
      setShowGeneratingIndicator(true);
      return;
    }
    if (!wasGenerating.current) {
      setShowGeneratingIndicator(false);
      return;
    }
    const timer = window.setTimeout(() => {
      wasGenerating.current = false;
      setShowGeneratingIndicator(false);
    }, 240);
    return () => window.clearTimeout(timer);
  }, [isGenerating]);
  // Work in flight can't be parked — `threadSettled` refuses to classify a
  // running or waiting chat as settled, so offering the control anyway would
  // be a button that visibly does nothing. Bringing one BACK is always fine.
  const canSettle =
    settled ||
    (thread.status !== "running" && thread.status !== "waiting_approval");
  const busyReason =
    thread.status === "waiting_approval" ? "waiting on you" : "still working";
  // Machine chip: only for threads pinned to ANOTHER machine.
  const localId = state.hello?.machineId;
  const isRemote =
    !!thread.machineId && !!localId && thread.machineId !== localId;
  const peer = isRemote
    ? state.peers.find((p) => p.machineId === thread.machineId)
    : undefined;
  const peerColor = peer ? (peer.colorOverride ?? peer.color) : undefined;
  // The gateway this chat belongs to — which outlives the switch away from it,
  // so a chat a local agent has taken back still knows whose it was.
  const gatewayId = hermesGatewayId(thread);
  // Only a chat Hermes actually holds wears its gateway's photo in place of
  // the brand mark; hand it to Claude and the mark goes back to telling you
  // truthfully who runs the next turn.
  const onHermes = hermesActive(thread);
  const hermesRec = gatewayId
    ? state.hermesAgents.find((a) => a.id === gatewayId)
    : undefined;
  const hermesAvatar = onHermes ? (hermesRec?.avatar ?? hermesRec?.image) : undefined;
  // Live presence of the thread's gateway (keyed by the registry id). Only
  // meaningful for a registered gateway; an orphaned thread has no record and
  // shows the brand mark with no dot.
  const hermesStatus = hermesRec && gatewayId ? state.hermesStatuses[gatewayId] : undefined;
  const gatewayName = hermesRec?.name ?? gatewayId;
  // Greyed in the Hermes view: the agent still owns this chat, but a local
  // agent is driving it right now. Never greyed in the workspace — there the
  // chat is simply live, badged or not.
  const dormant = inHermesView && !onHermes && !!gatewayId;
  // Same "appended modifier" trick as `nest`, so all four row variants pick it
  // up without another wrapper element.
  const dim = dormant ? " hermes-dormant" : "";
  const hermesPreview = useAvatarHoverPreview({
    image: hermesAvatar,
    name: hermesRec?.name ?? "hermes agent",
  });
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(thread.title);
  // "kebab" tracks the button; a point comes from a long-press or right-click
  // and is already in viewport coordinates. Touch needs the point mode: the
  // kebab is a 24px target that only appears on hover.
  const [menuAnchor, setMenuAnchor] = useState<"kebab" | MenuPoint | null>(
    null,
  );
  const [menuPos, setMenuPos] = useState<{ top: number; left: number } | null>(
    null,
  );
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [choosingFolder, setChoosingFolder] = useState(false);
  // Settling is one hover-height away from the star and the kebab, so the tick
  // asks before it parks: a small anchored "are you sure?" instead of acting
  // on the first click. Bringing a chat BACK stays one click (that direction
  // is harmless). null = closed, else the popup's fixed viewport position.
  const [settleConfirmPos, setSettleConfirmPos] = useState<{
    top: number;
    left: number;
  } | null>(null);
  const settleConfirmRef = useRef<HTMLDivElement | null>(null);
  const settleBtnRef = useRef<HTMLButtonElement>(null);
  const kebabRef = useRef<HTMLButtonElement>(null);
  // Written by `clampMenuIntoView`, so it has to be a mutable ref rather than
  // React's read-only `RefObject`.
  const menuRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const menuOpen = menuAnchor !== null;
  const longPressRef = useRef<{ disarm: () => void } | null>(null);

  // Floating log preview on hover. Suppressed while the kebab menu is open, a
  // rename is in progress, or a workspace drag is underway (state.dragProject),
  // per the card's own show/hide discipline.
  const hover = useHoverCard({
    render: () => <ThreadHoverCardBody thread={thread} />,
    disabled: menuOpen || editing || !!state.dragProject || settleConfirmPos !== null,
  });
  // Reactive read of the hover-populated preview cache so the card variant's
  // "N turns" hint appears the moment a hover fetch resolves.
  const cachedPreview = usePeekThreadPreview(thread);
  // Portraits live in localStorage, outside React's world, so a fresh upload
  // has to be pulled in by hand: bump a counter on the event and let the render
  // below re-resolve. Same shape as the APPEARANCE_EVENT re-reads in the
  // appearance blocks, minus the mirrored state (the answer is read inline).
  const [, bumpPortraits] = useState(0);
  useEffect(() => {
    const onEvt = () => bumpPortraits((n) => n + 1);
    window.addEventListener(PORTRAITS_EVENT, onEvt);
    return () => window.removeEventListener(PORTRAITS_EVENT, onEvt);
  }, []);

  function closeMenu() {
    setMenuAnchor(null);
    setConfirmDelete(false);
    setChoosingFolder(false);
    // The press has been consumed by the menu it opened, so the click guard
    // has no ghost left to swallow. Disarming here is what stops it eating
    // the user's NEXT tap — typically the settle tick on this very row,
    // about a finger-width from where they just pressed.
    longPressRef.current?.disarm();
  }

  // Long-press anywhere on the row opens the same menu the kebab does. This
  // is the only discoverable route to thread actions on a phone, and it
  // reuses the primitive the workspace headers already ship.
  const { disarm: disarmLongPress, ...longPress } = useLongPressMenu((point) => {
    if (editing) return;
    setMenuAnchor(point);
  }, !editing);
  longPressRef.current = { disarm: disarmLongPress };

  // Pull the menu back on screen once it has a real height. Runs on mount of
  // the portaled menu (ref callback) rather than in the positioning effect,
  // which fires before the menu exists and so can only ever measure 0.
  const clampMenuIntoView = (el: HTMLDivElement | null) => {
    menuRef.current = el;
    if (!el) return;
    const { height } = el.getBoundingClientRect();
    const safeBottom =
      Number.parseFloat(
        getComputedStyle(document.documentElement).getPropertyValue("--safe-b"),
      ) || 0;
    const maxTop = window.innerHeight - height - 8 - safeBottom;
    setMenuPos((pos) =>
      pos === null || pos.top <= maxTop ? pos : { ...pos, top: Math.max(4, maxTop) },
    );
  };

  // Anchor the portaled menu under the kebab. Both the sidebar anchor and the
  // portaled menu render unzoomed (only the message feed zooms), so the kebab's
  // viewport rect maps 1:1 onto the menu's fixed coordinates.
  useLayoutEffect(() => {
    if (menuAnchor === null) return;
    const width = 172;
    if (menuAnchor !== "kebab") {
      // Open at the pressed point; `clampMenuIntoView` corrects it against the
      // menu's real height once it mounts. A hardcoded reserve rots — this
      // menu is four items, and on touch each is 44px, so the old 160 put
      // Delete under the home indicator on any long-press near the bottom of
      // the list, which is exactly where the settled shelf lives.
      setMenuPos({
        top: Math.max(4, menuAnchor.y),
        left: Math.max(
          8,
          Math.min(menuAnchor.x, window.innerWidth - width - 8),
        ),
      });
      // A point-anchored menu has no element to track, so scrolling would
      // slide the row out from under it. Close instead of stranding it.
      const close = () => setMenuAnchor(null);
      window.addEventListener("scroll", close, true);
      return () => window.removeEventListener("scroll", close, true);
    }
    function place() {
      const b = kebabRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      const left = Math.max(8, r.right - width);
      setMenuPos({ top: r.bottom + 4, left });
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [menuAnchor]);

  // Close the menu on outside-click / Escape.
  useEffect(() => {
    if (!menuOpen) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (kebabRef.current?.contains(t)) return;
      if (menuRef.current?.contains(t)) return;
      closeMenu();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") closeMenu();
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  // Focus + select-all when the inline rename opens.
  useEffect(() => {
    if (!editing) return;
    const el = inputRef.current;
    if (el) {
      el.focus();
      el.select();
    }
  }, [editing]);

  function startRename() {
    closeMenu();
    setValue(thread.title);
    setEditing(true);
  }

  function commitRename() {
    if (!editing) return;
    const next = value.trim();
    setEditing(false);
    if (next && next !== thread.title)
      void actions.renameThread(thread.id, next);
  }

  // The server can refuse a settle that raced a turn starting (`canSettle`
  // only covers the steady state). Swallowing that leaves a control that
  // visibly did nothing, so say so where the user is already looking.
  // Close the settle confirm on outside-click, Escape, or any scroll (the row
  // slides out from under a fixed-position popup, same as point-anchored menus).
  useEffect(() => {
    if (settleConfirmPos === null) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (settleBtnRef.current?.contains(t)) return;
      if (settleConfirmRef.current?.contains(t)) return;
      setSettleConfirmPos(null);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setSettleConfirmPos(null);
    }
    const onScroll = () => setSettleConfirmPos(null);
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
    };
  }, [settleConfirmPos]);

  async function settleThread() {
    try {
      await actions.setThreadSettled(thread.id, !settled);
    } catch (err) {
      dispatch({
        type: "noticeAdd",
        notice: {
          id: Date.now(),
          threadId: thread.id,
          title: settled ? "Could not bring this chat back" : "Could not settle this chat",
          body: err instanceof Error ? err.message : String(err),
        },
      });
    }
  }

  function cancelRename() {
    setEditing(false);
    setValue(thread.title);
  }

  const card = view === "cards";
  // Only the card variant has room for a portrait, so the lookup is skipped
  // entirely for the list and shelf rows.
  const portrait = card ? resolvePortrait(thread.settings.model, thread.agent) : null;
  const hasStatusIndicator =
    !isGenerating &&
    (thread.status === "waiting_approval" || needsAttention);
  const statusEl = (
    <span
      className={`status-slot${hasStatusIndicator ? " has-indicator" : ""}`}
      title={isGenerating ? "Generating" : needsAttention ? "Unread activity" : undefined}
    >
      <span
        className={`status-dot st-${thread.status}${needsAttention ? " unread" : ""}`}
      />
    </span>
  );

  const markVisual = hermesAvatar ? (
    <span className="hermes-avatar-wrap">
      <span className="thread-row-avatar" {...hermesPreview.hoverProps}>
        <img src={hermesAvatar} alt="" />
      </span>
      <HermesPresenceDot status={hermesStatus} className="sm" />
    </span>
  ) : (
    <AgentMark agent={thread.agent} size={18} className="thread-row-mark" />
  );
  const markEl = (
    <span
      className={`agent-mark-slot${isGenerating ? " generating" : ""}${
        showGeneratingIndicator ? " transitioning" : ""
      }`}
    >
      <span className="agent-mark-visual">{markVisual}</span>
      {(isGenerating || showGeneratingIndicator) && (
        <span className="sidebar-loader">
          <LoaderIcon size={14} />
        </span>
      )}
    </span>
  );

  if (editing) {
    return (
      <div
        className={`thread-row editing${card ? " thread-card" : ""}${
          active ? " active" : ""
        }${needsAttention ? " has-attention" : ""}${isGenerating ? " is-generating" : ""}${nest}`}
      >
        {statusEl}
        {markEl}
        <input
          ref={inputRef}
          className="thread-rename-input"
          value={value}
          aria-label="Rename thread"
          onChange={(e) => setValue(e.target.value)}
          onClick={(e) => e.stopPropagation()}
          onBlur={commitRename}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") commitRename();
            else if (e.key === "Escape") cancelRename();
          }}
        />
      </div>
    );
  }

  // Shared bits so the list row and the card variant render identical behavior
  // (status, mark/avatar with its own hover, settle, star, kebab, and the
  // portaled menu): only their layout differs.
  const chipTitle = peer?.online
    ? `runs on ${peer.name}`
    : `on ${peer?.name ?? "another machine"} (offline)`;
  // Whose chat this is, shown only when it isn't yours and only once a second
  // person exists. Your own rows keep exactly the shape they always had, so
  // the badge reads as "somebody else's" rather than as new furniture.
  const author = thread.author ?? OWNER_PERSON_ID;
  const authorEl =
    state.people.length > 1 && author !== state.actingPerson ? (
      <PersonAvatar person={personById(state, author)} size={16} />
    ) : null;

  const chipsEl = (
    <>
      {/* Who is working this chat. In a workspace that is the gateway's name,
          so a card in the folder still reads "your agent has this". In the
          Hermes view the header already said the gateway, so the chip instead
          names the local agent that has taken the chat back. */}
      {!inHermesView && onHermes && gatewayName && (
        <span
          className="thread-chip hermes-chip"
          title={`worked by ${gatewayName}${hermesStatus ? ` — ${hermesPresence(hermesStatus).label}` : ""}`}
        >
          {gatewayName}
        </span>
      )}
      {dormant && (
        <span className="thread-chip hermes-chip off" title={`handed back to ${thread.agent}`}>
          on {thread.agent}
        </span>
      )}
      {isRemote && peerColor && (
        // Carries the machine's name on its own: the chip beside it is hidden
        // on phones, where its label cost more of the row than the title had.
        <span
          className="thread-chip-dot"
          style={{ background: peerColor }}
          title={chipTitle}
        />
      )}
      {isRemote && (
        <span className={`thread-chip${peer?.online ? "" : " off"}`} title={chipTitle}>
          {peer?.name ?? "remote"}
        </span>
      )}
    </>
  );
  const settleEl = (
    <>
      <button
        ref={settleBtnRef}
        type="button"
        className={`icon-btn thread-settle-btn${settleConfirmPos ? " on" : ""}`}
        disabled={!canSettle}
        aria-label={settled ? "Bring back to the list" : "Settle thread"}
        aria-haspopup={settled ? undefined : "dialog"}
        aria-expanded={settled ? undefined : settleConfirmPos !== null}
        title={
          settled
            ? "Bring back to the list"
            : canSettle
              ? "Settle — park this chat in the shelf"
              : `Can't settle: ${busyReason}`
        }
        onClick={(e) => {
          e.stopPropagation();
          if (!canSettle) return;
          // Bringing back is harmless — act at once. Settling asks first, so
          // a stray click beside the star can't park the chat unnoticed.
          if (settled) {
            void settleThread();
            return;
          }
          if (settleConfirmPos) {
            setSettleConfirmPos(null);
            return;
          }
          const r = e.currentTarget.getBoundingClientRect();
          const width = 200;
          setSettleConfirmPos({
            top: r.bottom + 4,
            left: Math.max(8, Math.min(r.right - width, window.innerWidth - width - 8)),
          });
        }}
      >
        {settled ? <UndoIcon size={13} /> : <CheckIcon size={13} />}
      </button>
      {settleConfirmPos &&
        createPortal(
          <div
            ref={settleConfirmRef}
            className="settle-confirm"
            role="alertdialog"
            aria-label="Confirm settle"
            style={{ top: settleConfirmPos.top, left: settleConfirmPos.left }}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="settle-confirm-text">Settle this chat?</div>
            <div className="settle-confirm-actions">
              <button
                type="button"
                className="settle-confirm-cancel"
                onClick={() => setSettleConfirmPos(null)}
              >
                cancel
              </button>
              <button
                type="button"
                className="settle-confirm-go"
                autoFocus
                onClick={() => {
                  setSettleConfirmPos(null);
                  void settleThread();
                }}
              >
                settle
              </button>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
  // Star toggle: appears on hover (opacity pattern, like the kebab) and stays
  // visible with a brass tint once favorited. stopPropagation so it never opens
  // the thread.
  const starEl = (
    <button
      type="button"
      className={`icon-btn thread-star${thread.favorite ? " on" : ""}`}
      aria-label={thread.favorite ? "Unfavorite thread" : "Favorite thread"}
      aria-pressed={!!thread.favorite}
      title={thread.favorite ? "Unfavorite" : "Favorite"}
      onClick={(e) => {
        e.stopPropagation();
        void actions.setThreadFavorite(thread.id, !thread.favorite);
      }}
    >
      <StarIcon size={14} filled={!!thread.favorite} />
    </button>
  );
  const kebabEl = (
    <button
      ref={kebabRef}
      type="button"
      className={`icon-btn thread-kebab${menuOpen ? " on" : ""}`}
      aria-label="Thread actions"
      aria-haspopup="menu"
      aria-expanded={menuOpen}
      title="Thread actions"
      onClick={(e) => {
        e.stopPropagation();
        setMenuAnchor((a) => (a === null ? "kebab" : null));
      }}
    >
      <MoreIcon size={15} />
    </button>
  );
  // The menu is a SIBLING of the row, not a child. React portals bubble
  // through the React tree, not the DOM tree — nested inside the row, every
  // tap on a menu item would pass through the row's onClickCapture, and the
  // long-press click guard would swallow the first one.
  const menuPortal =
    menuOpen && menuPos
      ? createPortal(
          <div
            ref={clampMenuIntoView}
            className="thread-menu"
            role="menu"
            style={{ top: menuPos.top, left: menuPos.left }}
            onClick={(e) => e.stopPropagation()}
          >
            {choosingFolder ? (
              <>
                <div className="thread-menu-title">Move to folder</div>
                <button
                  type="button"
                  role="menuitem"
                  className="thread-menu-item"
                  onClick={() => {
                    closeMenu();
                    onMoveToFolder?.(thread.id);
                  }}
                >
                  {!folderId ? <CheckIcon size={16} /> : <FolderIcon size={16} />}
                  <span>No folder</span>
                </button>
                {folders.map((folder) => (
                  <button
                    key={folder.id}
                    type="button"
                    role="menuitem"
                    className="thread-menu-item"
                    onClick={() => {
                      closeMenu();
                      onMoveToFolder?.(thread.id, folder.id);
                    }}
                  >
                    {folderId === folder.id ? (
                      <CheckIcon size={16} />
                    ) : (
                      <FolderIcon size={16} />
                    )}
                    <span>{folder.name}</span>
                  </button>
                ))}
                <button
                  type="button"
                  role="menuitem"
                  className="thread-menu-item divided"
                  onClick={() => {
                    closeMenu();
                    onAddFolder?.(thread.id);
                  }}
                >
                  <FolderPlusIcon size={16} />
                  <span>New folder…</span>
                </button>
                <button
                  type="button"
                  role="menuitem"
                  className="thread-menu-item"
                  onClick={() => setChoosingFolder(false)}
                >
                  <UndoIcon size={16} />
                  <span>Back</span>
                </button>
              </>
            ) : (
              <>
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item"
              disabled={!canSettle}
              onClick={() => {
                if (!canSettle) return;
                closeMenu();
                void settleThread();
              }}
            >
              {settled ? <UndoIcon size={16} /> : <CheckIcon size={16} />}
              <span>
                {settled
                  ? "Bring back"
                  : canSettle
                    ? "Settle"
                    : `Can't settle — ${busyReason}`}
              </span>
            </button>
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item"
              onClick={() => {
                closeMenu();
                void actions.setThreadFavorite(thread.id, !thread.favorite);
              }}
            >
              <StarIcon size={16} filled={!!thread.favorite} />
              <span>{thread.favorite ? "Unfavorite" : "Favorite"}</span>
            </button>
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item"
              onClick={startRename}
            >
              <PencilIcon size={16} />
              <span>Rename</span>
            </button>
            {onMoveToFolder && (
              <button
                type="button"
                role="menuitem"
                className="thread-menu-item"
                onClick={() => {
                  setConfirmDelete(false);
                  setChoosingFolder(true);
                }}
              >
                <FolderIcon size={16} />
                <span>Move to folder…</span>
              </button>
            )}
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item"
              onClick={() => {
                closeMenu();
                void actions.archiveThread(thread.id, thread.projectId);
              }}
            >
              <ArchiveIcon size={16} />
              <span>Archive</span>
            </button>
            <button
              type="button"
              role="menuitem"
              className={`thread-menu-item danger${confirmDelete ? " armed" : ""}`}
              onClick={() => {
                if (confirmDelete) {
                  closeMenu();
                  void actions.deleteThread(thread.id, thread.projectId);
                } else {
                  setConfirmDelete(true);
                }
              }}
            >
              <TrashIcon size={16} />
              <span>{confirmDelete ? "Delete permanently?" : "Delete"}</span>
            </button>
              </>
            )}
          </div>,
          document.body,
        )
      : null;

  const rowLabel = `${thread.title || "Untitled thread"}${
    needsAttention ? ", unread" : ""
  }${settled ? ", settled" : ""}`;
  // Long-press opens the same menu the kebab does, and a right-click opens it
  // where the pointer is. Both are the row's own gesture, so the card variant
  // carries them too.
  const rowGestures = {
    role: "button" as const,
    tabIndex: 0,
    "aria-label": rowLabel,
    ...longPress,
    // The row is its own drag handle — there is no grip, because a chat row is
    // mostly title and a grip would cost more width than it earns. Both
    // gestures get the press:
    //
    // - Mouse only. On touch the hold already belongs to the long-press menu,
    //   and the reorder hook arms its own 300ms hold on the same press, so
    //   attaching both would make one gesture mean two things. Touch keeps the
    //   menu; reordering is a pointer gesture until the menu grows a "move"
    //   entry of its own.
    // - The click guard still has to run on every device: it is what stops the
    //   click that ends a drag from also opening the chat.
    "data-reorder-id": thread.id,
    "data-drop": reorder?.drop,
    onPointerDown: (e: React.PointerEvent<HTMLElement>) => {
      longPress.onPointerDown(e);
      if (e.pointerType === "mouse") reorder?.handle.onPointerDown?.(e);
    },
    onClickCapture: (e: React.MouseEvent<HTMLElement>) => {
      longPress.onClickCapture(e);
      reorder?.handle.onClickCapture?.(e);
    },
    onClick: () => void actions.selectThread(thread.id),
    onKeyDown: (e: React.KeyboardEvent) =>
      e.key === "Enter" && void actions.selectThread(thread.id),
    onContextMenu: (e: React.MouseEvent) => {
      e.preventDefault();
      setMenuAnchor({ x: e.clientX, y: e.clientY });
    },
  };

  // Card variant: a bordered panel with a title row and a small meta row. The
  // title row is the title's alone — the machine chip and the row actions
  // live in the meta row, so a long title truncates against the card's full
  // width. The "N turns" hint only appears once a hover has cached this
  // thread's preview.
  if (card) {
    return (
      <>
        <div
          className={`thread-row thread-card long-press-menu${
            active ? " active" : ""
          }${needsAttention ? " has-attention" : ""}${recede ? " recede" : ""}${
            settled ? " slim" : ""
          }${settled && lit ? " lit" : ""}${isGenerating ? " is-generating" : ""}${nest}${dim}${reorder?.dragging ? " dragging" : ""}`}
          {...rowGestures}
          {...hover.hoverProps}
        >
          <div className="thread-card-top">
            {/* Decorative: the model already reads out through the chips and
                the mark, so the picture is hidden from assistive tech. Skins
                decide whether it is actually shown. */}
            {portrait && (
              <span className="fighter-portrait" aria-hidden="true">
                <img src={portrait} alt="" />
              </span>
            )}
            {statusEl}
            {markEl}
            {hermesPreview.portal}
            <span
              className="thread-card-title"
              title={thread.title || "Untitled thread"}
            >
              {thread.title || "Untitled thread"}
            </span>
          </div>
          <div className="thread-card-meta">
            {chipsEl}
            {authorEl}
            <span className="thread-row-time">{timeAgo(thread.updatedAt)}</span>
            {cachedPreview && cachedPreview.turnCount > 0 && (
              <span className="thread-card-turns">
                {cachedPreview.turnCount} {cachedPreview.turnCount === 1 ? "turn" : "turns"}
              </span>
            )}
            {settleEl}
            {starEl}
            {kebabEl}
          </div>
          {hover.portal}
        </div>
        {menuPortal}
      </>
    );
  }

  // Shelf rows stay single-line and dense. A live row gets two: the title
  // owns the first, and the machine chip, clock, and row actions share the
  // second — a long title truncates against the row's full width instead of
  // fighting the chips and buttons for the scraps. (Compact view is the
  // density-first opt-in, so it keeps the one-line row.)
  const twoLine = !settled && view === "list";
  if (twoLine) {
    return (
      <>
        <div
          className={`thread-row two-line long-press-menu${active ? " active" : ""}${
            needsAttention ? " has-attention" : ""
          }${recede ? " recede" : ""}${isGenerating ? " is-generating" : ""}${nest}${dim}${reorder?.dragging ? " dragging" : ""}`}
          {...rowGestures}
          {...hover.hoverProps}
        >
          <div className="thread-row-main">
            {statusEl}
            {markEl}
            {hermesPreview.portal}
            <span
              className="thread-row-title"
              title={thread.title || "Untitled thread"}
            >
              {thread.title || "Untitled thread"}
            </span>
          </div>
          <div className="thread-row-sub">
            {chipsEl}
            {authorEl}
            <span className="thread-row-time">{timeAgo(thread.updatedAt)}</span>
            {settleEl}
            {starEl}
            {kebabEl}
          </div>
          {hover.portal}
        </div>
        {menuPortal}
      </>
    );
  }

  return (
    <>
      <div
        className={`thread-row long-press-menu${active ? " active" : ""}${
          needsAttention ? " has-attention" : ""
        }${recede ? " recede" : ""}${settled ? " slim" : ""}${
          settled && lit ? " lit" : ""
        }${isGenerating ? " is-generating" : ""}${nest}${dim}${reorder?.dragging ? " dragging" : ""}`}
        {...rowGestures}
        {...hover.hoverProps}
      >
        {statusEl}
        {markEl}
        {hermesPreview.portal}
        <span
          className="thread-row-title"
          title={thread.title || "Untitled thread"}
        >
          {thread.title || "Untitled thread"}
        </span>
        {chipsEl}
        {authorEl}
        <span className="thread-row-time">{timeAgo(thread.updatedAt)}</span>
        {settleEl}
        {starEl}
        {kebabEl}
        {hover.portal}
      </div>
      {menuPortal}
    </>
  );
}

function WorkspaceSection({
  workspace,
  primary,
  members,
  threads,
  lastActivity,
  collapsed,
  forceOpen,
  renaming,
  autoSettleDays,
  now,
  hideHeader = false,
  pageSize = THREAD_PAGE_SIZE,
  reorder,
  showLocalBadge,
  view,
  onToggle,
  onRenameDone,
  onMenu,
  onNewThread,
  chatFolders,
  chatFolderAssignments,
  onMoveChatToFolder,
  onAddChatFolder,
  onReorderThreads,
  threadOrder,
  sidebarScrollRef,
}: {
  workspace: Workspace;
  /** First locally-present member project — target for pop-out and removal.
   *  Undefined when every member lives on another machine. */
  primary: Project | undefined;
  members: MemberView[];
  threads: Thread[];
  /** Days of silence before a chat settles itself; null disables that. */
  autoSettleDays: number | null;
  /** Clock quantized to the minute by the parent, so the settled/active split
   *  is stable across renders instead of drifting mid-frame. */
  now: number;
  collapsed: boolean;
  forceOpen: boolean;
  renaming: boolean;
  /** Picker layout renders the chosen project's chats with no header — the
   *  picker bar above the list is the header. */
  hideHeader?: boolean;
  /** How many active chats to show before "load more". Larger when this is
   *  the only project on screen. */
  pageSize?: number;
  /** Drag-to-reorder wiring for the header's grip. Absent where the layout
   *  has nothing to reorder (one project on screen) or a search is on. */
  reorder?: {
    handle: ReorderHandleProps;
    dragging: boolean;
    drop?: "before" | "after";
  };
  /** True last activity: max updatedAt across the workspace's threads,
   *  independent of the favorite-first display order. */
  lastActivity: string | undefined;
  /** Show the subtle "this machine" chip on the header (pinLocal on + this
   *  workspace has a root on the local machine). */
  showLocalBadge: boolean;
  /** Sidebar presentation, forwarded to each thread row. */
  view: SidebarLayout["view"];
  /** Accordion layout owns which project is open, so it overrides the
   *  section's own collapse dispatch. */
  onToggle?: () => void;
  onRenameDone: () => void;
  onMenu: (point: MenuPoint, workspace: Workspace) => void;
  /** New-thread entry point; the parent shows a machine→root picker when
   *  the workspace has several roots. */
  onNewThread: (e: React.MouseEvent) => void;
  chatFolders: ChatFolder[];
  chatFolderAssignments: Record<string, string>;
  onMoveChatToFolder: (threadId: string, folderId?: string) => void;
  onAddChatFolder: (threadId?: string) => void;
  /** Commit a dragged chat order (the ids of this list, top to bottom). */
  onReorderThreads: (ids: string[]) => void;
  /** The stored manual chat order, applied to this list's active chats. */
  threadOrder: string[];
  /** The sidebar's scroller, so a drag that reaches the top or bottom edge
   *  carries the list with it. The chat list itself does not scroll. */
  sidebarScrollRef: React.RefObject<HTMLDivElement | null>;
}) {
  const { state, dispatch, actions } = useStore();
  const [visibleThreadCount, setVisibleThreadCount] = useState(pageSize);
  // Drag-to-reorder for the chats in this section. Scoped to this list's own
  // container rather than the sidebar scroller, because the workspace sections
  // run the same hook over that scroller and both look for [data-reorder-id] —
  // one container per list is what keeps a chat drag from picking up a
  // workspace, and vice versa.
  const threadListRef = useRef<HTMLDivElement | null>(null);
  const threadDrag = useReorderDrag({
    containerRef: threadListRef,
    scrollRef: sidebarScrollRef,
    onCommit: onReorderThreads,
    enabled: !renaming,
  });
  // useState only reads its argument once, so switching layout in Settings
  // would otherwise leave a section paged at the old size until a reload —
  // a picker showing 5 chats with half a screen of blank space under a
  // "load more". Grow to the new floor; never shrink past what the user
  // already expanded to.
  const lastPageSizeRef = useRef(pageSize);
  if (lastPageSizeRef.current !== pageSize) {
    lastPageSizeRef.current = pageSize;
    setVisibleThreadCount((count) => Math.max(count, pageSize));
  }
  const [name, setName] = useState(workspace.name);
  // Set when this workspace belongs to a remote server we are a guest on.
  // Also gates the local-only actions below: renaming, restyling or attaching
  // a root to somebody else's workspace is their machine's business, and those
  // requests do not route, so offering them would only produce an error.
  const guestServer = workspaceServer(state, workspace);
  const workspacePreview = useAvatarHoverPreview({
    image: workspace.image,
    name: workspace.name,
  });
  // Header hover card: the roots (machine + folder + online dot), thread count,
  // and last activity - replacing the old native title tooltip on the name.
  const headerHover = useHoverCard({
    render: () => (
      <WorkspaceHoverCardBody
        name={workspace.name}
        members={members.map((m) => ({
          machineLabel: m.machineLabel,
          path: m.path,
          online: m.online,
        }))}
        threadCount={threads.length}
        lastActivity={lastActivity}
      />
    ),
    // Also suppressed while a workspace drag is in progress.
    disabled: renaming || !!state.dragProject,
    width: 300,
  });
  const renameRef = useRef<HTMLInputElement>(null);
  const open = forceOpen || !collapsed;

  // The split that makes the list finite. Active chats keep a STATIC order
  // (newest-created first) so a row never moves under the cursor while an
  // agent works; settled chats order by when they were parked, because that
  // is what you scan the shelf by.
  const { parents, workersOf } = useMemo(
    () => groupDispatchChildren(threads),
    [threads],
  );
  const { active, settled } = useSettledSplit(parents, autoSettleDays, now);
  // Chats sit where they were dragged. Applied to the split's output because
  // that is what actually reaches a row: the static creation-date sort inside
  // it would otherwise overwrite any order imposed further up. The shelf keeps
  // its own parked-at order — a settled chat is filed, not arranged.
  const orderedActive = useMemo(
    () => applyThreadOrder(active, threadOrder),
    [active, threadOrder],
  );

  const shown = forceOpen
    ? orderedActive
    : pageActiveThreads(
        orderedActive,
        visibleThreadCount,
        (t) =>
          threadNeedsAttention(state, t) ||
          state.activeThreadId === t.id ||
          isRecentlyKeptActive(t, now),
      );
  const remaining = active.length - shown.length;
  const chatFolderIds = new Set(chatFolders.map((folder) => folder.id));
  const rootShown = shown.filter((thread) => {
    const assigned = chatFolderAssignments[thread.id];
    return !assigned || !chatFolderIds.has(assigned);
  });
  const solo = !!state.solo;
  // `disarm` is destructured out rather than spread: the rest of the bag is
  // DOM event props, and React would warn about (and emit) a stray `disarm`
  // attribute on the div.
  const { disarm: _disarmWorkspacePress, ...longPress } = useLongPressMenu(
    (point) => onMenu(point, workspace),
    !renaming,
  );

  useEffect(() => {
    if (!renaming) return;
    setName(workspace.name);
    const el = renameRef.current;
    if (el) {
      el.focus();
      el.select();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [renaming]);

  function commitRename() {
    const next = name.trim();
    onRenameDone();
    if (next && next !== workspace.name) {
      void actions.renameWorkspace(workspace.id, next);
    }
  }

  return (
    <div
      className={`project-section${reorder?.dragging ? " dragging" : ""}`}
      data-reorder-id={workspace.id}
      data-drop={reorder?.drop}
    >
      {hideHeader ? null : (
      <div
        className={`project-head long-press-menu${
          workspace.hidden ? " hidden-ws" : ""
        }`}
        {...longPress}
        // The row's own drag pops the project out into its own window, which
        // is a different gesture from the grip's reorder — never both at once.
        draggable={!solo && !renaming && !!primary && !reorder?.dragging}
        onDragStart={(e) => {
          if (!primary) return;
          e.dataTransfer.setData("text/plain", primary.id);
          e.dataTransfer.effectAllowed = "copyMove";
          dispatch({ type: "dragProject", project: primary });
        }}
        onDragEnd={() => dispatch({ type: "dragProject", project: null })}
        onContextMenu={(e) => {
          e.preventDefault();
          onMenu({ x: e.clientX, y: e.clientY }, workspace);
        }}
      >
        {reorder && !renaming && (
          // A grip rather than the whole row: the row already spends its drag
          // on popping the project out to its own window.
          <span
            className="project-grip"
            role="button"
            tabIndex={-1}
            aria-label={`Drag to move ${workspace.name}`}
            title="Drag to reorder"
            draggable={false}
            {...reorder.handle}
          >
            <GripIcon size={12} />
          </span>
        )}
        {renaming ? (
          <input
            ref={renameRef}
            className="thread-rename-input workspace-rename-input"
            value={name}
            aria-label="Rename workspace"
            onChange={(e) => setName(e.target.value)}
            onClick={(e) => e.stopPropagation()}
            onBlur={commitRename}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") commitRename();
              else if (e.key === "Escape") onRenameDone();
            }}
          />
        ) : (
          <button
            className="project-toggle"
            onClick={() =>
              onToggle
                ? onToggle()
                : dispatch({ type: "toggleProject", projectId: workspace.id })
            }
            {...headerHover.hoverProps}
          >
            <FolderClosedIcon size={13} className="project-folder-icon" />
            <span className="project-name">{workspace.name}</span>
            {/* Whose machine this lives on, when it is not ours. A guest link's
                workspaces sit in the same list as our own — they are work we
                actually do — but which box a chat will run on is not something
                to leave the reader to infer from the roots. */}
            {guestServer && (
              <span className="project-server-chip" title={`on ${guestServer.name}`}>
                {guestServer.name}
              </span>
            )}
            {workspace.favorite && (
              <StarIcon size={11} filled className="project-fav-star" />
            )}
            {/* Only reachable while "show hidden" is on (or a search pulled it
                back): says WHY this row is greyed, so a revealed shelf reads as
                stashed rather than broken. */}
            {workspace.hidden && (
              <EyeIcon size={11} off className="project-hidden-mark" />
            )}
            {/* Counts what is actually in play. The shelf carries its own
                number, so the header stops reading "69" forever. */}
            {/* While searching this counts every match, shelf included —
                otherwise a query that only hits parked chats renders
                rows under a header reading 0. */}
            <span className="project-count">
              {countLabel(active.length, settled.length, forceOpen, open)}
            </span>
          </button>
        )}
        {workspacePreview.portal}
        {headerHover.portal}
        {showLocalBadge && !renaming && (
          // "You are here": a root of this workspace lives on this machine.
          // Reuses the thread-chip language, tinted with the brass accent.
          <span className="thread-chip project-local-chip" title="A folder of this workspace is on this machine">
            this machine
          </span>
        )}
        {!solo && primary && (
          <button
            className="icon-btn"
            aria-label="Open in its own window"
            title="Open in its own window (or drag the row out)"
            onClick={(e) => {
              e.stopPropagation();
              void openProjectWindow(primary);
            }}
          >
            <PopoutIcon size={13} />
          </button>
        )}
        {!solo && primary && (
          <DangerButton
            label="Remove project"
            onConfirm={() => void actions.deleteProject(primary.id)}
          />
        )}
      </div>
      )}
      {(open || hideHeader) && (
        <div className="project-threads" ref={threadListRef}>
          {shown.length === 0 && settled.length === 0 && (
            <div className="project-empty">
              {forceOpen ? "no matches" : "no threads yet"}
            </div>
          )}
          {rootShown.map((t) => (
            <Fragment key={t.id}>
              <ThreadRow
                thread={t}
                active={state.activeThreadId === t.id}
                view={view}
                folders={chatFolders}
                onMoveToFolder={onMoveChatToFolder}
                onAddFolder={(threadId) => onAddChatFolder(threadId)}
                reorder={{
                  handle: threadDrag.handleProps(t.id),
                  dragging: threadDrag.draggingId === t.id,
                  drop: threadDrag.dropId === t.id ? threadDrag.dropSide : undefined,
                }}
              />
              <DispatchWorkers
                workers={workersOf.get(t.id) ?? []}
                forceOpen={forceOpen}
                view={view}
              />
            </Fragment>
          ))}
          {chatFolders.map((folder) => {
            const folderThreads = shown.filter(
              (thread) => chatFolderAssignments[thread.id] === folder.id,
            );
            const total = active.filter(
              (thread) => chatFolderAssignments[thread.id] === folder.id,
            ).length;
            const collapseKey = `chat-folder:${folder.id}`;
            const folderOpen = forceOpen || !state.collapsed[collapseKey];
            return (
              <div className="chat-folder" key={folder.id}>
                <button
                  type="button"
                  className="chat-folder-head"
                  aria-expanded={folderOpen}
                  onClick={() =>
                    dispatch({ type: "toggleProject", projectId: collapseKey })
                  }
                >
                  <FolderClosedIcon size={18} />
                  <span>{folder.name}</span>
                  <ChevronIcon size={11} open={folderOpen} className="row-chevron" />
                </button>
                {folderOpen && (
                  <div className="chat-folder-threads">
                    {folderThreads.length === 0 && total === 0 && (
                      <div className="chat-folder-empty">empty</div>
                    )}
                    {folderThreads.map((thread) => (
                      <Fragment key={thread.id}>
                        <ThreadRow
                          thread={thread}
                          active={state.activeThreadId === thread.id}
                          view={view}
                          folders={chatFolders}
                          folderId={folder.id}
                          onMoveToFolder={onMoveChatToFolder}
                          onAddFolder={(threadId) => onAddChatFolder(threadId)}
                        />
                        <DispatchWorkers
                          workers={workersOf.get(thread.id) ?? []}
                          forceOpen={forceOpen}
                          view={view}
                        />
                      </Fragment>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
          {remaining > 0 && (
            <button
              className="load-more-threads"
              onClick={() =>
                setVisibleThreadCount((count) => count + THREAD_PAGE_SIZE)
              }
            >
              <span>load more</span>
              <span className="load-more-count">
                {Math.min(THREAD_PAGE_SIZE, remaining)} of {remaining}
              </span>
            </button>
          )}
          {/* The shelf: collapsed to a single line by default, so parked work
              costs one row of sidebar instead of N. Hidden entirely when
              nothing is parked, and skipped while searching (the matches are
              already flattened into the list above). */}
          <SettledShelf
            threads={settled}
            forceOpen={forceOpen}
            view={view}
            folders={chatFolders}
            folderAssignments={chatFolderAssignments}
            onMoveToFolder={onMoveChatToFolder}
            onAddFolder={(threadId) => onAddChatFolder(threadId)}
          />
        </div>
      )}
    </div>
  );
}

/** One Hermes gateway's collapsible thread group (own component so paging
 *  state isn't a hook-in-a-loop). */
function HermesGroup({
  id,
  name,
  image,
  known,
  threads,
  forceOpen,
  autoSettleDays,
  now,
  view,
}: {
  id: string;
  name: string;
  image?: string;
  /** False for the trailing bucket of threads whose gateway was removed. */
  known: boolean;
  threads: Thread[];
  forceOpen: boolean;
  autoSettleDays: number | null;
  now: number;
  /** Sidebar presentation, forwarded to each thread row. */
  view: SidebarLayout["view"];
}) {
  const { state, dispatch, actions } = useStore();
  const [visibleThreadCount, setVisibleThreadCount] =
    useState(THREAD_PAGE_SIZE);
  const agentPreview = useAvatarHoverPreview({ image, name });
  // Live presence, shown only for a real registry gateway (never the trailing
  // "removed agents" bucket, which has no gateway to poll).
  const status = known ? state.hermesStatuses[id] : undefined;
  const presence = known ? hermesPresence(status) : null;
  const collapseKey = `hermes:${id}`;
  const open = forceOpen || !state.collapsed[collapseKey];

  // Same split as a workspace section: a gateway you talk to daily
  // accumulates just as much history as a repo does.
  const { active, settled } = useSettledSplit(threads, autoSettleDays, now);

  const shown = forceOpen
    ? active
    : pageActiveThreads(
        active,
        visibleThreadCount,
        (t) =>
          threadNeedsAttention(state, t) ||
          state.activeThreadId === t.id ||
          isRecentlyKeptActive(t, now),
      );
  const remaining = active.length - shown.length;
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  // `disarm` is pulled out so it never reaches the DOM via the spread below.
  const { disarm: _disarmHermes, ...longPress } = useLongPressMenu(
    (point) => setMenu(point),
    known,
  );
  // Rolled up onto the header so a collapsed group still shows it is busy or
  // wants a look.
  const activity = projectActivity(state, threads);

  return (
    <div className="project-section">
      <div
        className="project-head long-press-menu"
        {...longPress}
        onContextMenu={(e) => {
          if (!known) return;
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        <button
          className="project-toggle"
          onClick={() =>
            dispatch({ type: "toggleProject", projectId: collapseKey })
          }
        >
          <ChevronIcon size={12} open={open} className="row-chevron" />
          <span className="hermes-avatar-wrap">
            <span
              className={`sidebar-avatar agent-avatar${image ? " has-image" : ""}`}
              {...agentPreview.hoverProps}
            >
              {image ? (
                <img src={image} alt="" />
              ) : (
                <AgentMark agent="hermes" size={12} />
              )}
            </span>
            {known && <HermesPresenceDot status={status} />}
          </span>
          {agentPreview.portal}
          <span
            className={`project-name hermes-agent-name${
              presence?.kind === "offline" ? " hermes-offline" : ""
            }`}
            title={presence ? `${name}: ${presence.title}` : name}
          >
            {name}
          </span>
          <ProjectPulse activity={activity} />
          {/* While searching this counts every match, shelf included —
                otherwise a query that only hits parked chats renders
                rows under a header reading 0. */}
          <span className="project-count">
            {countLabel(active.length, settled.length, forceOpen, open)}
          </span>
        </button>
        {known && (
          <button
            className="icon-btn"
            aria-label={`New chat with ${name}`}
            title={`New chat with ${name}`}
            onClick={() => actions.openHermesDraft(id)}
          >
            <PlusIcon size={13} />
          </button>
        )}
      </div>
      {open && (
        <div className="project-threads">
          {shown.length === 0 && settled.length === 0 && (
            <div className="project-empty">
              {forceOpen ? "no matches" : "no chats yet"}
            </div>
          )}
          {shown.map((t) => (
            <ThreadRow
              key={t.id}
              thread={t}
              active={state.activeThreadId === t.id}
              inHermesView
              view={view}
            />
          ))}
          {remaining > 0 && (
            <button
              className="load-more-threads"
              onClick={() =>
                setVisibleThreadCount((count) => count + THREAD_PAGE_SIZE)
              }
            >
              <span>load more</span>
              <span className="load-more-count">
                {Math.min(THREAD_PAGE_SIZE, remaining)} of {remaining}
              </span>
            </button>
          )}
          <SettledShelf
            threads={settled}
            forceOpen={forceOpen}
            inHermesView
            view={view}
          />
        </div>
      )}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            {
              label: image ? "Change profile picture…" : "Add profile picture…",
              icon: <PencilIcon size={13} />,
              onSelect: () => {
                void pickAvatarImage()
                  .then((next) =>
                    next ? actions.setHermesAgentAvatar(id, next) : undefined,
                  )
                  .catch((error: unknown) => {
                    window.alert(
                      error instanceof Error ? error.message : String(error),
                    );
                  });
              },
            },
            ...(image
              ? [
                  {
                    label: "Remove profile picture",
                    icon: <TrashIcon size={13} />,
                    danger: true,
                    onSelect: () => void actions.setHermesAgentAvatar(id, null),
                  },
                ]
              : []),
          ]}
        />
      )}
    </div>
  );
}

/** The sidebar's dedicated agents view: one group per registry gateway holding
 *  every chat BOUND to it, regardless of which project the chat was born in
 *  and regardless of who is running its turns right now — lit while the
 *  gateway has it, greyed once a local agent has taken it back.
 *
 *  A workspace chat therefore appears both here and in its folder. That is the
 *  point, not a leak: the folder answers "what is this work part of" and this
 *  answers "who is doing it", and the two views are shown one at a time (see
 *  `SidebarView`), so no chat is ever drawn twice on screen. */
function HermesSection({
  filter,
  contentMatches,
  searchingContent,
  autoSettleDays,
  now,
  view,
}: {
  filter: string;
  contentMatches: ReadonlySet<string>;
  searchingContent: boolean;
  autoSettleDays: number | null;
  now: number;
  view: SidebarLayout["view"];
}) {
  const { state } = useStore();
  const gateways = useMemo(
    () => state.hello?.agents.find((a) => a.id === "hermes")?.models ?? [],
    [state.hello],
  );
  // Registry records carry the user-set avatar; hello's model list is the
  // fallback for the gateway-advertised image.
  const recById = useMemo(
    () => new Map(state.hermesAgents.map((a) => [a.id, a])),
    [state.hermesAgents],
  );
  const hermesThreads = useMemo(() => {
    const arr = Object.values(state.threads)
      .flat()
      // Every chat BOUND to a gateway, not just the ones Hermes is holding
      // right now: handing one back to a local agent greys its row here, it
      // does not remove it. Losing the row would lose the only place the chat
      // can be handed forward again.
      .filter((t) => hermesActive(t) || hermesDormant(t))
      .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
    // Favorites float first within each gateway group; the per-group filter
    // below preserves this order (stable sort).
    arr.sort((a, b) => (a.favorite ? 0 : 1) - (b.favorite ? 0 : 1));
    return arr;
  }, [state.threads]);
  if (gateways.length === 0 && hermesThreads.length === 0) {
    return (
      <div className="sidebar-empty">
        <p>No Hermes agents yet.</p>
        <p>Add one in Settings → Agents.</p>
      </div>
    );
  }

  const groups = [
    ...gateways.map((g) => ({
      id: g.id,
      name: g.name,
      image: recById.get(g.id)?.avatar ?? recById.get(g.id)?.image ?? g.image,
      known: true,
      // Grouped by the persisted binding, not by settings.model: a chat a
      // local agent has taken back carries THAT agent's model id in `model`.
      threads: hermesThreads.filter((t) => hermesGatewayId(t) === g.id),
    })),
  ];
  const orphaned = hermesThreads.filter(
    (t) => !gateways.some((g) => g.id === hermesGatewayId(t)),
  );
  if (orphaned.length > 0) {
    groups.push({
      id: "removed",
      name: "removed agents",
      image: undefined,
      known: false,
      threads: orphaned,
    });
  }

  const shown = !filter
    ? groups
    : groups
        .map((g) =>
          g.name.toLowerCase().includes(filter)
            ? g
            : {
                ...g,
                threads: g.threads.filter((t) =>
                  threadMatches(t, filter, contentMatches),
                ),
              },
        )
        .filter(
          (g) => g.name.toLowerCase().includes(filter) || g.threads.length > 0,
        );
  if (filter && shown.length === 0) {
    return (
      <div className="sidebar-empty">
        <p>
          {searchingContent
            ? "Searching thread content…"
            : "No matching agent threads."}
        </p>
      </div>
    );
  }

  return (
    <div className="hermes-band">
      <div className="hermes-band-label">Hermes agents</div>
      {shown.map((g) => (
        <HermesGroup
          key={g.id}
          id={g.id}
          name={g.name}
          image={g.image}
          known={g.known}
          threads={g.threads}
          forceOpen={!!filter}
          autoSettleDays={autoSettleDays}
          now={now}
          view={view}
        />
      ))}
    </div>
  );
}

/** Workspace roots manager: list every root (machine + folder), detach, and
 *  attach a new root on any online machine (folder browse proxied to it). */
function WorkspaceRootsModal({
  workspace,
  members,
  onClose,
}: {
  workspace: Workspace;
  members: MemberView[];
  onClose: () => void;
}) {
  const { state, actions } = useStore();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** Non-null → the folder browser is open for this machine. */
  const [pickingOn, setPickingOn] = useState<{
    machineId: string;
    label: string;
  } | null>(null);

  const localId = state.hello?.machineId ?? "";
  const machines: { machineId: string; label: string; online: boolean }[] = [
    {
      machineId: localId,
      label: state.hello?.friendlyName ?? "this machine",
      online: true,
    },
    ...state.peers.map((p) => ({
      machineId: p.machineId,
      label: p.name,
      online: !!p.online,
    })),
  ];

  async function detach(m: MemberView) {
    setError(null);
    setBusy(true);
    try {
      await actions.detachRoot(
        workspace.id,
        m.machineId || localId,
        m.projectId,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal roots-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>“{workspace.name}” — machines & folders</span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        <div className="roots-list">
          {members.map((m) => (
            <div key={`${m.machineId}:${m.projectId}`} className="settings-row">
              <span className="settings-value">
                <span className={`peer-dot${m.online ? " online" : ""}`} />
                {m.rootLabel}
                <span className="dim"> · {m.machineLabel}</span>
              </span>
              {m.projectId === workspace.id ? (
                <span
                  className="settings-value dim"
                  title="The workspace's original folder — it anchors the workspace"
                >
                  original
                </span>
              ) : (
                <button
                  type="button"
                  className="settings-toggle"
                  disabled={busy}
                  title="Remove this folder from the workspace (nothing is deleted — it becomes its own workspace on its machine)"
                  onClick={() => void detach(m)}
                >
                  remove
                </button>
              )}
            </div>
          ))}
        </div>

        <div className="settings-label">add a folder</div>
        <div className="roots-attach">
          {machines.map((mc) => (
            <button
              key={mc.machineId}
              type="button"
              className="settings-toggle"
              disabled={busy || !mc.online}
              title={
                mc.online
                  ? `Browse ${mc.label}'s folders`
                  : `${mc.label} is offline`
              }
              onClick={() =>
                setPickingOn({ machineId: mc.machineId, label: mc.label })
              }
            >
              on {mc.label}
              {mc.online ? "" : " (offline)"}
            </button>
          ))}
        </div>

        {error && <div className="modal-error">{error}</div>}

        {pickingOn && (
          <DirPicker
            title={`Add a folder on ${pickingOn.label}`}
            confirmLabel="Add this folder"
            machineId={
              pickingOn.machineId !== localId ? pickingOn.machineId : undefined
            }
            onClose={() => setPickingOn(null)}
            onPick={(path) => {
              setError(null);
              setBusy(true);
              void actions
                .attachRoot(workspace.id, pickingOn.machineId, path)
                .catch((e: unknown) =>
                  setError(e instanceof Error ? e.message : String(e)),
                )
                .finally(() => setBusy(false));
            }}
          />
        )}
      </div>
    </div>
  );
}

/** View-options popover anchored under the filter button in the search row.
 *  Portaled + fixed-positioned like the thread kebab menu so it escapes the
 *  sidebar's scroll/clip; closes on outside-click and Escape. */
function SidebarViewPopover({
  anchorRef,
  layout,
  hiddenCount,
  update,
  onClose,
}: {
  anchorRef: React.RefObject<HTMLButtonElement>;
  layout: SidebarLayout;
  /** How many workspaces are stashed. Zero means the reveal row is not drawn. */
  hiddenCount: number;
  update: (patch: Partial<SidebarLayout>) => void;
  onClose: () => void;
}) {
  const popRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);

  // Anchor under the button, right-aligned and clamped into the viewport.
  // (Sidebar anchor and portaled panel both render unzoomed, so the rect maps
  //  1:1 onto the panel's fixed coordinates - see the thread menu.)
  useLayoutEffect(() => {
    function place() {
      const b = anchorRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      const width = 236;
      const left = Math.max(8, r.right - width);
      setPos({ top: r.bottom + 6, left });
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [anchorRef]);

  useEffect(() => {
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (anchorRef.current?.contains(t)) return;
      if (popRef.current?.contains(t)) return;
      onClose();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [anchorRef, onClose]);

  if (!pos) return null;

  return createPortal(
    <div
      ref={popRef}
      className="thread-menu sidebar-view-pop"
      role="dialog"
      aria-label="Sidebar view options"
      style={{ top: pos.top, left: pos.left }}
    >
      <div className="sidebar-view-seg-row">
        <span className="sidebar-view-label">View</span>
        <div className="settings-seg">
          <button
            type="button"
            className={`settings-toggle${layout.view === "list" ? " on" : ""}`}
            onClick={() => update({ view: "list" })}
          >
            List
          </button>
          <button
            type="button"
            className={`settings-toggle${layout.view === "compact" ? " on" : ""}`}
            onClick={() => update({ view: "compact" })}
          >
            Compact
          </button>
          <button
            type="button"
            className={`settings-toggle${layout.view === "cards" ? " on" : ""}`}
            onClick={() => update({ view: "cards" })}
          >
            Cards
          </button>
        </div>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={layout.showTimes}
        className={`sidebar-view-row${layout.showTimes ? " on" : ""}`}
        onClick={() => update({ showTimes: !layout.showTimes })}
      >
        <span>Show timestamps</span>
        <span className="sidebar-view-switch" />
      </button>
      <button
        type="button"
        role="switch"
        aria-checked={layout.showAgents}
        className={`sidebar-view-row${layout.showAgents ? " on" : ""}`}
        onClick={() => update({ showAgents: !layout.showAgents })}
      >
        <span>Show AI agent on chats</span>
        <span className="sidebar-view-switch" />
      </button>
      <button
        type="button"
        role="switch"
        aria-checked={layout.bigNames}
        className={`sidebar-view-row${layout.bigNames ? " on" : ""}`}
        onClick={() => update({ bigNames: !layout.bigNames })}
      >
        <span>Larger workspace names</span>
        <span className="sidebar-view-switch" />
      </button>
      <button
        type="button"
        role="switch"
        aria-checked={layout.pinLocal}
        className={`sidebar-view-row${layout.pinLocal ? " on" : ""}`}
        onClick={() => update({ pinLocal: !layout.pinLocal })}
      >
        <span>Keep this machine on top</span>
        <span className="sidebar-view-switch" />
      </button>
      {/* Only offered once there is something to reveal — a switch that can
          only ever do nothing is worse than no switch. The rail has its own
          one-tap stash; this is how the OTHER three layouts (and anyone who
          wants the whole shelf back in place at once) reach the same thing. */}
      {hiddenCount > 0 && (
        <button
          type="button"
          role="switch"
          aria-checked={layout.showHidden}
          className={`sidebar-view-row${layout.showHidden ? " on" : ""}`}
          onClick={() => update({ showHidden: !layout.showHidden })}
        >
          <span>Show hidden projects ({hiddenCount})</span>
          <span className="sidebar-view-switch" />
        </button>
      )}
    </div>,
    document.body,
  );
}

/** Invisible col-resize handle on the sidebar's right edge. Drives width live
 *  via setWidthLive during the drag and persists once on release; double-click
 *  resets. Hidden at the mobile breakpoint (the sidebar is an overlay there). */
function SidebarResizeHandle({
  width,
  setWidthLive,
  onCommit,
}: {
  width: number;
  setWidthLive: (w: number) => void;
  onCommit: (w: number) => void;
}) {
  const drag = useRef<{ startX: number; startW: number } | null>(null);
  // Mirror the prop so pointer-up reads the final width without a stale closure.
  const latest = useRef(width);
  latest.current = width;

  function onPointerDown(e: React.PointerEvent<HTMLDivElement>) {
    if (e.button !== 0) return; // primary button only
    e.preventDefault();
    drag.current = { startX: e.clientX, startW: latest.current };
    e.currentTarget.setPointerCapture(e.pointerId);
    // Suppress text selection app-wide while dragging.
    document.body.classList.add("sidebar-resizing");
  }

  function onPointerMove(e: React.PointerEvent<HTMLDivElement>) {
    const d = drag.current;
    if (!d) return;
    setWidthLive(d.startW + (e.clientX - d.startX));
  }

  function endDrag(e: React.PointerEvent<HTMLDivElement>) {
    if (!drag.current) return;
    drag.current = null;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      // Capture may already be released - safe to ignore.
    }
    document.body.classList.remove("sidebar-resizing");
    // Persist the final width once, so a drag doesn't spam localStorage.
    onCommit(latest.current);
  }

  return (
    <div
      className="sidebar-resize"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      title="Drag to resize · double-click to reset"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onDoubleClick={() => onCommit(SIDEBAR_WIDTH_DEFAULT)}
    />
  );
}

/** Full-screen conversation search, opened from the sidebar's Search button.
 *  A blurred backdrop over a clean field; results are threads (across the
 *  current project or all projects) each rendered with its agent mark. */
function ThreadSearchModal({ onClose }: { onClose: () => void }) {
  const { state, actions } = useStore();
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState<"project" | "all">("project");
  const [scopeOpen, setScopeOpen] = useState(false);
  const [closing, setClosing] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  // Play the exit animation, then actually unmount. 160ms matches the CSS.
  const close = () => {
    setClosing(true);
    window.setTimeout(onClose, 160);
  };

  useEffect(() => {
    inputRef.current?.focus();
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setClosing(true);
        window.setTimeout(onClose, 160);
      }
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  // "Current project" is wherever the open chat lives, else the pending draft's
  // home, else the first project in the fleet.
  const active = state.activeThreadId
    ? findThread(state, state.activeThreadId)
    : null;
  const currentProjectId =
    active?.projectId ?? state.draft?.projectId ?? state.projects[0]?.id ?? null;

  const projectName = (id: string) => {
    if (isQuickHomeProjectId(id)) return "Quick threads";
    if (id === HERMES_HOME_PROJECT_ID) return "Agents";
    return state.projects.find((p) => p.id === id)?.name ?? "Project";
  };

  const q = query.trim().toLowerCase();
  const results = useMemo(() => {
    const all: Thread[] = [];
    for (const pid of Object.keys(state.threads)) {
      if (scope === "project" && pid !== currentProjectId) continue;
      for (const t of state.threads[pid]) all.push(t);
    }
    const matched = q
      ? all.filter((t) => (t.title || "untitled thread").toLowerCase().includes(q))
      : all;
    return matched
      .slice()
      .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1))
      .slice(0, 60);
  }, [state.threads, scope, currentProjectId, q]);

  function openThread(id: string) {
    void actions.selectThread(id);
    close();
  }

  const scopeLabel = scope === "project" ? "This project" : "All projects";

  return createPortal(
    <div
      className={`search-modal-backdrop${closing ? " closing" : ""}`}
      onMouseDown={close}
    >
      <div
        className="search-modal"
        role="dialog"
        aria-label="Search conversations"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="search-modal-field">
          <SearchIcon size={18} className="search-modal-glyph" />
          <input
            ref={inputRef}
            type="text"
            placeholder="Search conversations…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onFocus={() => setScopeOpen(false)}
          />
          <div className="search-scope">
            <button
              type="button"
              className="search-scope-toggle"
              aria-haspopup="menu"
              aria-expanded={scopeOpen}
              onClick={() => setScopeOpen((v) => !v)}
            >
              <span>{scopeLabel}</span>
              <ChevronIcon size={12} open={scopeOpen} className="row-chevron" />
            </button>
            {scopeOpen && (
              <div className="search-scope-menu" role="menu">
                <button
                  type="button"
                  className={scope === "project" ? "on" : ""}
                  onClick={() => {
                    setScope("project");
                    setScopeOpen(false);
                  }}
                >
                  This project
                </button>
                <button
                  type="button"
                  className={scope === "all" ? "on" : ""}
                  onClick={() => {
                    setScope("all");
                    setScopeOpen(false);
                  }}
                >
                  All projects
                </button>
              </div>
            )}
          </div>
        </div>
        <div
          className="search-modal-results"
          onMouseDown={() => setScopeOpen(false)}
        >
          {results.length === 0 ? (
            <div className="search-modal-empty">
              {q ? "No conversations found." : "No conversations yet."}
            </div>
          ) : (
            results.map((t) => (
              <button
                key={t.id}
                type="button"
                className="search-result"
                onClick={() => openThread(t.id)}
              >
                <AgentMark
                  agent={t.agent}
                  size={18}
                  className="search-result-mark"
                />
                <span className="search-result-title">
                  {t.title || "Untitled thread"}
                </span>
                <span className="search-result-project">
                  {projectName(t.projectId)}
                </span>
                <span className="search-result-time">{timeAgo(t.updatedAt)}</span>
              </button>
            ))
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}

export const Sidebar = memo(function Sidebar({
  onAddProject,
  onOpenSchedules,
}: {
  onAddProject: () => void;
  onOpenSchedules: () => void;
}) {
  const { state, dispatch, actions } = useStore();
  const [query, setQuery] = useState("");
  // Per-device notification subscriptions live outside React (localStorage,
  // shared with solo windows), so read them as an external store.
  const notifyPrefs = useSyncExternalStore(subscribeNotifyPrefs, getNotifyPrefs);
  // Presentation prefs (view mode, big names, pin-local, width). Instantiated
  // here and threaded down; the width also drives the --sidebar-w CSS var.
  const { layout, update: updateLayout, setWidthLive } = useSidebarLayout();
  const [filterOpen, setFilterOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);
  const filterBtnRef = useRef<HTMLButtonElement>(null);
  const storedView = useSyncExternalStore(subscribeSidebarView, getSidebarView);
  // A stored "agents" only takes effect once the Hermes surfaces are eligible;
  // an install that has since removed its gateways falls back to the fleet
  // without losing the stored preference.
  const view: SidebarView =
    storedView === "agents" && !showHermesAgents() ? "fleet" : storedView;
  const switchView = (v: SidebarView) => {
    setSidebarView(v);
    // Keep the pane and destination list in agreement. Quick Threads and Hermes
    // are global homes, so neither should leave a workspace thread stranded
    // beneath its list (or vice versa).
    const active = state.activeThreadId
      ? findThread(state, state.activeThreadId)
      : null;
    const activeProjectId = active?.projectId ?? state.draft?.projectId;
    // Which lists the open chat appears in — plural, because a workspace chat
    // bound to a Hermes agent now legitimately appears in two. Closing it on
    // the way into either one would take away the chat you switched views to
    // work on. Only a chat that is in NEITHER list gets cleared.
    const activeKinds: SidebarView[] = !activeProjectId
      ? []
      : isQuickHomeProjectId(activeProjectId)
        ? ["quick"]
        : activeProjectId === HERMES_HOME_PROJECT_ID
          ? ["agents"]
          : [
              "fleet",
              ...(active && (hermesActive(active) || hermesDormant(active))
                ? (["agents"] as const)
                : []),
            ];
    if (v === "agents") {
      // Jump straight to a Hermes chat that wants attention; otherwise leave a
      // Hermes chat (or nothing) in place, and clear only a workspace one — we
      // never auto-open a chat when none is asking for it.
      const needy = hermesAttentionThreads(state);
      if (needy.length > 0) {
        if (needy[0].id !== state.activeThreadId)
          void actions.selectThread(needy[0].id);
      } else if (activeKinds.length > 0 && !activeKinds.includes("agents")) {
        dispatch({ type: "closeActive" });
      }
    } else if (activeKinds.length > 0 && !activeKinds.includes(v)) {
      dispatch({ type: "closeActive" });
    }
  };
  const [showSettings, setShowSettings] = useState(false);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    workspace: Workspace;
    primary: Project | undefined;
  } | null>(null);
  /** "Where should this thread run?" picker shown from the "+" button. Stores
   *  only the anchor + which workspace it belongs to; the member roots are
   *  resolved fresh on every render (see the render site) so peer online flags
   *  flow live into the menu while it is open. */
  const [newThreadMenu, setNewThreadMenu] = useState<{
    x: number;
    y: number;
    workspaceId: string;
  } | null>(null);
  const [rootsModal, setRootsModal] = useState<Workspace | null>(null);
  /** First-thread-on-a-new-machine flow: pick the folder there, then draft. */
  const [setupFolder, setSetupFolder] = useState<{
    workspaceId: string;
    machineId: string;
    machineLabel: string;
  } | null>(null);

  // Re-render every minute so "2h ago" stays honest. Doubles as the clock the
  // settled/active split reads: quantizing to a minute keeps the split stable
  // between ticks instead of recomputing against a moving Date.now().
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(t);
  }, []);

  const [sidebarPrefs, setSidebarPrefs] =
    useState<SidebarPrefs>(getSidebarPrefs);
  useEffect(() => {
    const onPrefs = (e: Event) =>
      setSidebarPrefs((e as CustomEvent<SidebarPrefs>).detail);
    window.addEventListener(SIDEBARPREFS_EVENT, onPrefs);
    return () => window.removeEventListener(SIDEBARPREFS_EVENT, onPrefs);
  }, []);

  const filter = query.trim().toLowerCase();
  const searchableThreads = useMemo(
    () => Object.values(state.threads).flat(),
    [state.threads],
  );
  const searchableThreadIds = useMemo(
    () => searchableThreads.map((thread) => thread.id),
    [searchableThreads],
  );
  const [contentSearch, setContentSearch] = useState<{
    query: string;
    ids: ReadonlySet<string>;
  }>({ query: "", ids: new Set() });
  const [searchingContent, setSearchingContent] = useState(false);
  useEffect(() => {
    if (!filter) {
      setContentSearch({ query: "", ids: new Set() });
      setSearchingContent(false);
      return;
    }
    let cancelled = false;
    setSearchingContent(true);
    const timer = window.setTimeout(() => {
      void actions
        .searchThreads(filter, searchableThreadIds)
        .then((threadIds) => {
          if (!cancelled) {
            setContentSearch({ query: filter, ids: new Set(threadIds) });
            setSearchingContent(false);
          }
        })
        .catch(() => {
          // Title search remains useful if a peer goes offline or an older
          // Threadknot does not yet implement content search.
          if (!cancelled) {
            setContentSearch({ query: filter, ids: new Set() });
            setSearchingContent(false);
          }
        });
    }, 180);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [actions, filter, searchableThreadIds]);
  const contentMatches =
    contentSearch.query === filter ? contentSearch.ids : NO_CONTENT_MATCHES;
  const soloId = state.solo;
  // This machine's id, for the new-thread menu's local-vs-remote branching:
  // DirPicker and openDraft only take a machineId when it is NOT this machine.
  const localId = state.hello?.machineId ?? "";
  // Solo windows are project-dedicated — the agents view only exists in the
  // fleet window.
  const agentsView = showHermesAgents() && view === "agents" && !soloId;
  const quickView = view === "quick" && !soloId;
  const quickThreads = useMemo(
    () =>
      Object.values(state.threads)
        .flat()
        .filter((thread) => isQuickHomeProjectId(thread.projectId))
        .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1)),
    [state.threads],
  );
  const matchingQuickThreads = useMemo(
    () =>
      filter
        ? quickThreads.filter((thread) =>
            threadMatches(thread, filter, contentMatches),
          )
        : quickThreads,
    [quickThreads, filter, contentMatches],
  );
  const openProjectId = state.activeThreadId
    ? findThread(state, state.activeThreadId)?.projectId
    : state.draft?.projectId;
  // Quick Threads is already the place for starting a folderless chat. Keep a
  // draft ready whenever that destination has no quick thread selected, so the
  // pane opens on the composer instead of asking for a second "new thread"
  // click. A layout effect prevents the redundant empty pane from flashing
  // while switching destinations; the machine id dependency retries after the
  // initial hello handshake if Quick Threads was restored on launch.
  useLayoutEffect(() => {
    if (
      !quickView ||
      (openProjectId !== undefined && isQuickHomeProjectId(openProjectId))
    ) {
      return;
    }
    actions.openQuickDraft();
  }, [actions, openProjectId, quickView, state.hello?.machineId]);
  useEffect(() => {
    if (soloId || !openProjectId) return;
    // The global homes pull the list over to match the pane. A workspace
    // project no longer does so unconditionally: a Hermes-bound chat is listed
    // under both its workspace and the agents view, so opening one from the
    // agents view has to leave you there — the composer reads this view to know
    // that sending means "hand it back to its agent".
    const destinationView: SidebarView | null = isQuickHomeProjectId(openProjectId)
      ? "quick"
      : openProjectId === HERMES_HOME_PROJECT_ID
        ? "agents"
        : view === "agents"
          ? null
          : "fleet";
    if (!destinationView || view === destinationView) return;
    setSidebarView(destinationView);
  }, [openProjectId, soloId, view]);
  // Hermes chats asking to be looked at — badges the fleet-view button so a
  // finished turn / pending approval is visible without leaving the workspaces.
  const hermesAttention = useMemo(
    () => (showHermesAgents() ? hermesAttentionThreads(state).length : 0),
    [state],
  );
  // A newer master exists than the build that is running. Pulses regardless of
  // whether this machine can act on it cleanly: knowing you are behind is the
  // point, and the Updates tab explains what is blocking the fix.
  // A built-but-not-loaded binary counts too: the work is done and one click
  // away, so going quiet there would strand the machine on the old version.
  const updateReady =
    state.update?.updateAvailable === true ||
    state.update?.restartPending === true;
  const updateHint = state.update?.restartPending
    ? "Rebuilt and ready — restart Threadknot to load it"
    : state.update
      ? `v${state.update.runningVersion} → master v${state.update.masterVersion ?? "?"} (${Math.max(state.update.behindBy, state.update.headBehind)} behind)`
      : "";

  const projectById = useMemo(
    () => new Map(allProjects(state).map((p) => [p.id, p])),
    [state.projects, state.serverCatalogs],
  );

  // Workspaces are the sidebar's top level. Until the workspace list arrives
  // (connect race, or an old server) synthesize one section per uncovered
  // project so the sidebar never blanks.
  const sections = useMemo(() => {
    // Quick Threads homes render as the dedicated destination (the rail's
    // plus tile), never as a workspace. Servers from before that rule may
    // still replicate a same-name workspace wrapping the home; strip those
    // memberships here so the fleet can never show the home twice.
    // The MERGED list: this machine's own workspaces plus whatever each remote
    // server is showing us. `allWorkspaces` is a read-time merge — the guest
    // catalogs are never folded into `state.workspaces`, because that is the
    // list our own peers receive on their next connect.
    const workspaces = allWorkspaces(state)
      .map((w) => ({
        ...w,
        members: w.members.filter((m) => !isQuickHomeProjectId(m.projectId)),
      }))
      .filter((w) => w.members.length > 0);
    const covered = new Set(
      workspaces.flatMap((w) => w.members.map((m) => m.projectId)),
    );
    const synthetic: Workspace[] = allProjects(state)
      // The Hermes home project renders as the dedicated section, never as a
      // workspace.
      .filter(
        (p) =>
          p.id !== HERMES_HOME_PROJECT_ID &&
          !isQuickHomeProjectId(p.id) &&
          !covered.has(p.id),
      )
      .map((p) => ({
        id: p.id,
        name: p.name,
        createdAt: p.createdAt,
        updatedAt: p.createdAt,
        members: [{ machineId: "", projectId: p.id }],
      }));
    return [...workspaces, ...synthetic];
  }, [state.workspaces, state.projects, state.serverCatalogs, state.servers]);

  /** Members resolved for display + their threads (local AND remote roots,
   *  merged newest first). In solo mode only the solo project's slice of the
   *  workspace is shown. */
  const sectionData = useMemo(() => {
    const localId = state.hello?.machineId;
    const map = new Map<
      string,
      {
        projects: Project[];
        members: MemberView[];
        threads: Thread[];
        lastActivity: string | undefined;
      }
    >();
    for (const w of sections) {
      const raw = soloId
        ? w.members.filter((m) => m.projectId === soloId)
        : w.members;
      const members: MemberView[] = raw.map((m) => {
        const isLocal =
          !localId || m.machineId === localId || m.machineId === "";
        const p = projectById.get(m.projectId);
        const peer = state.peers.find((x) => x.machineId === m.machineId);
        // A server we are a guest on is never in `peers`. Reading liveness from
        // the peer table alone reported a box we are connected to as an offline
        // "remote machine", which disabled every row that pointed at it.
        const server = peer
          ? undefined
          : state.servers.find((x) => x.machineId === m.machineId);
        return {
          machineId: m.machineId,
          projectId: m.projectId,
          rootLabel:
            p?.name ??
            m.name ??
            m.path?.split("/").filter(Boolean).pop() ??
            "folder",
          machineLabel: isLocal
            ? (state.hello?.friendlyName ?? "this machine")
            : (peer?.name ?? server?.name ?? "remote machine"),
          isLocal,
          online: isLocal || !!peer?.online || !!server?.online,
          path: p?.path ?? m.path ?? "",
        };
      });
      const projects = members
        .filter((m) => m.isLocal)
        .map((m) => projectById.get(m.projectId))
        .filter((p): p is Project => !!p);
      // Hermes chats are NOT filtered out. A chat handed to a Hermes agent is
      // still this workspace's work — it keeps its card here (badged with the
      // gateway, see ThreadRow) and appears a second time in the Hermes view.
      // Deliberately two places, one chat: which folder it belongs to and who
      // is working it are different questions. Only the folderless Hermes home
      // is exclusive to that view, and its project never reaches `members`.
      // The people filter lands here, on the one memo every section, card and
      // count downstream reads from. `threadInView` is always true until a
      // second person exists, so a single-person install computes the same
      // list it always did.
      const threads = members
        .flatMap((m) => state.threads[m.projectId] ?? [])
        .filter((t) => threadInView(state.viewPerson, t))
        .sort((a, b) => (a.updatedAt < b.updatedAt ? 1 : -1));
      // The workspace's true last activity is the max updatedAt across its
      // threads, computed BEFORE the favorite re-sort scrambles the order. An
      // old favorited thread must not make an active workspace look stale.
      const lastActivity = threads.reduce<string | undefined>(
        (max, t) => (!max || t.updatedAt > max ? t.updatedAt : max),
        undefined,
      );
      // Float favorited threads first, preserving recency within each group
      // (Array.sort is stable). Cards view shares this same data.
      threads.sort((a, b) => (a.favorite ? 0 : 1) - (b.favorite ? 0 : 1));
      // The manual drag order is NOT applied here: `useSettledSplit` re-sorts
      // the active chats by creation date further down, so anything imposed at
      // this level is discarded before it reaches a row. It is applied to that
      // sort's output instead, in the section.
      map.set(w.id, { projects, members, threads, lastActivity });
    }
    return map;
  }, [sections, soloId, projectById, state.threads, state.peers, state.hello, state.viewPerson, state.serverCatalogs, layout.threadOrder]);

  // Projects sit where they were PUT. The base order used to be activity —
  // the workspace holding the newest thread floated to the top — which meant
  // the list rearranged itself under the cursor exactly when it was busiest:
  // you reached for a project and an agent finishing a turn somewhere else
  // slid a different one under the click. Activity still SHOWS, on the status
  // light and the unread badge; it just no longer moves anything.
  //
  // What is left are stable passes, lowest precedence first so the last wins:
  //   (3) pinLocal  ->  (2) favorite  ->  (1) manual drag order
  //
  // Manual order sits ON TOP, above the pin-local and favorite floats it used
  // to sit under. Where you dropped something is the most explicit statement
  // of intent there is, and under the old precedence dragging a workspace with
  // no root on this machine visibly snapped back the moment you let go. The
  // two floats still order everything you have never dragged.
  const visibleWorkspaces = useMemo(() => {
    const pool = soloId
      ? sections.filter((w) => w.members.some((m) => m.projectId === soloId))
      : sections;
    const shown = !filter
      ? pool
      : pool.filter((w) => {
          if (w.name.toLowerCase().includes(filter)) return true;
          return (sectionData.get(w.id)?.threads ?? []).some((t) =>
            threadMatches(t, filter, contentMatches),
          );
        });

    // Stashed projects drop out — unless you asked to see them, or a search is
    // running. A search is an explicit request for a named thing, and quietly
    // returning "no results" for a chat that exists in a project you put away
    // months ago is how you conclude the chat is gone. Hidden matches come back
    // marked (`.hidden-ws`), not silently.
    // A solo window is already scoped to one project and has no rail or filter
    // popover to unhide from, so hiding never applies there.
    const visible =
      soloId || filter || layout.showHidden
        ? shown
        : shown.filter((w) => !w.hidden);

    // (3) Float workspaces that have a root on this machine above purely-remote
    // ones. A member counts as local when its machineId matches ours or is ""
    // (see MemberView.isLocal). Array sort is stable, so the incoming order is
    // preserved within each group.
    const isLocalWs = (w: Workspace) =>
      (sectionData.get(w.id)?.members ?? []).some((m) => m.isLocal);
    const byLocal = layout.pinLocal
      ? [...visible].sort((a, b) => (isLocalWs(a) ? 0 : 1) - (isLocalWs(b) ? 0 : 1))
      : visible;

    // (2) Favorited workspaces float above the rest.
    const byFavorite = [...byLocal].sort(
      (a, b) => (a.favorite ? 0 : 1) - (b.favorite ? 0 : 1),
    );

    // (1) Manual drag order last, so it wins: ids listed in workspaceOrder come
    // first in that order; ids absent from it keep the float order above.
    const ordered = applyProjectOrder(byFavorite, layout.workspaceOrder);

    // (0) A revealed stash sinks, and this outranks even the manual order:
    // "show hidden" asks to SEE the shelf, not to put the shelf back in the
    // list. Everything you actually work in stays exactly where it was and the
    // projects you put away collect at the bottom. Search results are left
    // interleaved — there the ordering that matters is the match, not the list.
    return filter
      ? ordered
      : [...ordered].sort((a, b) => (a.hidden ? 1 : 0) - (b.hidden ? 1 : 0));
  }, [
    filter,
    soloId,
    sections,
    sectionData,
    contentMatches,
    layout.pinLocal,
    layout.showHidden,
    layout.workspaceOrder,
  ]);

  /** Everything currently stashed, in the order the sidebar would list it.
   *  Drives the rail's stash tile and its bring-back menu; empty means the
   *  affordance never renders, so nobody who has hidden nothing sees it. */
  const hiddenWorkspaces = useMemo(
    () =>
      soloId
        ? []
        : applyProjectOrder(
            sections.filter((w) => w.hidden),
            layout.workspaceOrder,
          ),
    [sections, soloId, layout.workspaceOrder],
  );

  // A search hides projects, and a solo window shows one slice of one — in
  // neither case is what is on screen the list being ordered, so dragging is
  // off until the sidebar is showing the whole fleet again.
  const reorderEnabled = !filter && !soloId;
  // One place the order lives: the same `workspaceOrder` the sidebar sorts by.
  // Ids that are not on screen right now (a peer's workspaces before its
  // replica loads) are kept rather than pruned — dropping them would silently
  // reset their position — but they park at the end, since a drag cannot say
  // where among the visible ones they belong.
  const commitOrder = useCallback(
    (ids: string[]) =>
      updateLayout({
        workspaceOrder: mergeProjectOrder(ids, layout.workspaceOrder),
      }),
    [updateLayout, layout.workspaceOrder],
  );
  // Chats dragged within one workspace's list. Merged into the same flat order
  // as every other workspace's: thread ids are unique, and a drag can only ever
  // rearrange the list it happened in, so one list needs no per-workspace
  // bookkeeping. Ids that have scrolled out of the page limit, or belong to a
  // workspace that is not on screen, are preserved by the merge.
  const commitThreadOrder = useCallback(
    (ids: string[]) =>
      updateLayout({ threadOrder: mergeProjectOrder(ids, layout.threadOrder) }),
    [updateLayout, layout.threadOrder],
  );
  // Header-grip reordering for the layouts that stack every project down the
  // sidebar. The rail runs its own drag over its own scroller; the picker
  // shows one project at a time and so has nothing to reorder.
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const scrollbarIdleTimer = useRef<number | null>(null);
  const onSidebarScroll = useCallback(() => {
    const scroller = scrollRef.current;
    if (!scroller) return;
    scroller.classList.add("is-scrolling");
    if (scrollbarIdleTimer.current !== null) {
      window.clearTimeout(scrollbarIdleTimer.current);
    }
    // A short quiet window covers wheel/trackpad event spacing without leaving
    // WebKitGTK's composited thumb hanging over whatever opens next.
    scrollbarIdleTimer.current = window.setTimeout(() => {
      scroller.classList.remove("is-scrolling");
      scrollbarIdleTimer.current = null;
    }, 160);
  }, []);
  useEffect(
    () => () => {
      if (scrollbarIdleTimer.current !== null) {
        window.clearTimeout(scrollbarIdleTimer.current);
      }
    },
    [],
  );

  // Settings is portaled above the sidebar, but WebKitGTK can still composite
  // the native scrollbar over it. Hide the thumb synchronously on open even if
  // it falls inside the final scroll-event quiet window.
  useEffect(() => {
    if (!showSettings) return;
    if (scrollbarIdleTimer.current !== null) {
      window.clearTimeout(scrollbarIdleTimer.current);
      scrollbarIdleTimer.current = null;
    }
    scrollRef.current?.classList.remove("is-scrolling");
  }, [showSettings]);
  const sectionDrag = useReorderDrag({
    containerRef: scrollRef,
    scrollRef,
    onCommit: commitOrder,
    enabled: reorderEnabled,
  });

  // A search has to reach every project, so it always falls back to the
  // all-open layout regardless of the preference — otherwise "accordion" and
  // "picker" would quietly scope the search to whichever project was showing.
  // Solo windows are already one project, so the project layer is moot there.
  const projectLayout: ProjectLayout =
    filter || soloId ? "sections" : sidebarPrefs.projectLayout;
  // The rail is navigation chrome, not part of the list: it stays put while a
  // search flattens every project into one result set below it, rather than
  // vanishing and reflowing the sidebar on the first keystroke.
  const railMode =
    sidebarPrefs.projectLayout === "rail" && !agentsView && !soloId;

  // Which project the single-project layouts are showing. Null means "not
  // chosen yet", which resolves to the project holding the open chat — so
  // opening Threadknot lands you where you were working, not on an arbitrary one.
  const [pickedId, setPickedId] = useState<string | null>(null);
  const [pickerMenu, setPickerMenu] = useState<MenuPoint | null>(null);
  const [folderDialog, setFolderDialog] = useState<{
    workspaceId: string;
    moveThreadId?: string;
  } | null>(null);
  /** Where the rail's stash tile was clicked; non-null renders the bring-back
   *  menu. Portaled from here with the sidebar's other menus. */
  const [stashMenu, setStashMenu] = useState<MenuPoint | null>(null);

  const moveChatToFolder = useCallback(
    (threadId: string, folderId?: string) => {
      const next = { ...layout.chatFolderAssignments };
      if (folderId) next[threadId] = folderId;
      else delete next[threadId];
      updateLayout({ chatFolderAssignments: next });
    },
    [layout.chatFolderAssignments, updateLayout],
  );

  const createChatFolder = useCallback(
    (name: string, workspaceId: string, moveThreadId?: string) => {
      const id = `folder-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const chatFolders = [...layout.chatFolders, { id, workspaceId, name }];
      const chatFolderAssignments = moveThreadId
        ? { ...layout.chatFolderAssignments, [moveThreadId]: id }
        : layout.chatFolderAssignments;
      updateLayout({ chatFolders, chatFolderAssignments });
      setFolderDialog(null);
    },
    [layout.chatFolderAssignments, layout.chatFolders, updateLayout],
  );
  const activeWorkspaceId = useMemo(() => {
    const active = state.activeThreadId
      ? findThread(state, state.activeThreadId)
      : null;
    const activeProjectId = active?.projectId ?? state.draft?.projectId;
    if (!activeProjectId) return null;
    for (const [id, data] of sectionData) {
      if (data.members.some((member) => member.projectId === activeProjectId)) return id;
    }
    return null;
  }, [state, sectionData]);
  const shownWorkspaceId =
    (pickedId && visibleWorkspaces.some((w) => w.id === pickedId)
      ? pickedId
      : null) ??
    (activeWorkspaceId &&
    visibleWorkspaces.some((w) => w.id === activeWorkspaceId)
      ? activeWorkspaceId
      : null) ??
    visibleWorkspaces[0]?.id ??
    null;
  // Picker renders exactly one project; the others render every section and
  // differ only in which are expanded.
  // Picker and rail both render exactly one project; the others render every
  // section and differ only in which are expanded.
  const laidOutWorkspaces =
    projectLayout === "picker" || projectLayout === "rail"
      ? visibleWorkspaces.filter((w) => w.id === shownWorkspaceId)
      : visibleWorkspaces;
  const pickedWorkspace =
    visibleWorkspaces.find((w) => w.id === shownWorkspaceId) ?? null;
  const threadsByWorkspace = useMemo(
    () => new Map([...sectionData].map(([id, d]) => [id, d.threads])),
    [sectionData],
  );

  // The center-screen new-thread empty state is not a destination. Whenever a
  // workspace is the selected destination but no chat or draft is open, put a
  // draft for that workspace in the pane immediately. Prefer this machine's
  // root, then any reachable root; if the workspace only exists on an offline
  // peer, leave navigation alone until that peer can actually host the draft.
  //
  // Held until `state.restored`: the workspace list arrives well before the
  // startup restore has reopened the remembered chat, and firing in that
  // window drops a draft in whichever workspace happens to sort first — which
  // is what a reload used to land on.
  useLayoutEffect(() => {
    if (
      !state.restored ||
      quickView ||
      agentsView ||
      state.activeThreadId ||
      state.draft ||
      !shownWorkspaceId
    ) {
      return;
    }
    const members = sectionData.get(shownWorkspaceId)?.members ?? [];
    const target = members.find((member) => member.isLocal) ?? members.find((member) => member.online);
    if (target) actions.openDraft(target.projectId, target.machineId || undefined);
  }, [
    actions,
    agentsView,
    quickView,
    sectionData,
    shownWorkspaceId,
    state.activeThreadId,
    state.draft,
    state.restored,
  ]);

  /** Tapping a project on the rail switches to it AND opens something in it.
   *  Switching the list alone left the previous project's chat filling the
   *  screen, so the rail said one project and the pane showed another. */
  function pickRailProject(id: string) {
    if (quickView || agentsView) switchView("fleet");
    setPickedId(id);
    const threads = sectionData.get(id)?.threads ?? [];
    // Already reading something in this project (a re-tap, or the chat that
    // made this project current): stay put rather than yanking the pane onto
    // a different chat.
    if (threads.some((t) => t.id === state.activeThreadId)) return;
    const target = landingThread(
      state,
      threads,
      sidebarPrefs.autoSettleDays,
      now,
    );
    if (target) void actions.selectThread(target.id);
    // A project with no chats yet has nothing to land on, and leaving the last
    // project's chat up is exactly the confusion this fixes.
    else dispatch({ type: "closeActive" });
  }

  /** Put a project away. Nothing is deleted — its roots, chats and running
   *  agents are untouched — but if it is the one on screen the sidebar is about
   *  to stop listing it, and leaving its chat filling the pane is the same
   *  "rail says one thing, pane shows another" mismatch `pickRailProject`
   *  exists to prevent. So hand over to the first project still showing. */
  function hideWorkspace(id: string) {
    void actions.setWorkspaceHidden(id, true);
    if (layout.showHidden || id !== shownWorkspaceId) return;
    const next = visibleWorkspaces.find((w) => w.id !== id && !w.hidden);
    if (next) pickRailProject(next.id);
    else {
      setPickedId(null);
      dispatch({ type: "closeActive" });
    }
  }

  /** Bring one back and go straight to it — you asked for it by name, so
   *  landing anywhere else would mean hunting for it again. */
  function unhideWorkspace(id: string) {
    void actions.setWorkspaceHidden(id, false);
    pickRailProject(id);
  }

  // Shared by the per-project header's + button and by the picker/rail bar.
  // Those two layouts hide the header, and the + lived only there — so
  // "new thread" was unreachable from inside a workspace in either one.
  //
  // Always the menu, even for a workspace with a single root: it is what
  // makes "add a new project folder" discoverable, and the menu resolves the
  // roots live rather than from a snapshot taken when it opened.
  function startNewThread(workspaceId: string, point: MenuPoint) {
    setNewThreadMenu({ x: point.x, y: point.y, workspaceId });
  }

  // The "New chat" button, resolved against wherever you are: a quick thread in
  // the Quick/Agents homes, otherwise a thread in the current project. A single
  // online root drafts straight in; multiple roots open the machine/folder menu.
  function startNewChatHere(e: React.MouseEvent) {
    const active = state.activeThreadId
      ? findThread(state, state.activeThreadId)
      : null;
    const activePid = active?.projectId ?? state.draft?.projectId;
    if (quickView || agentsView || (activePid && isQuickHomeProjectId(activePid))) {
      actions.openQuickDraft();
      return;
    }
    let wsId = pickedWorkspace?.id ?? shownWorkspaceId ?? null;
    if (!wsId && activePid) {
      for (const [id, d] of sectionData) {
        if (d.members.some((m) => m.projectId === activePid)) {
          wsId = id;
          break;
        }
      }
    }
    wsId = wsId ?? visibleWorkspaces[0]?.id ?? null;
    if (!wsId) {
      actions.openQuickDraft();
      return;
    }
    const members = sectionData.get(wsId)?.members ?? [];
    const online = members.filter((m) => m.online);
    if (online.length === 1) {
      actions.openDraft(online[0].projectId, online[0].machineId || undefined);
      return;
    }
    const r = e.currentTarget.getBoundingClientRect();
    startNewThread(wsId, { x: r.left, y: r.bottom + 4 });
  }

  return (
    <>
    <aside
      className={`sidebar${state.sidebarOpen ? " open" : ""}${
        layout.collapsed ? " collapsed" : ""
      }${railMode ? " layout-rail" : ""}${
        layout.view === "cards" ? " cards-view" : ""
      }${layout.view === "compact" ? " compact-view" : ""}${
        layout.bigNames ? " big-names" : ""
      }${layout.showTimes ? "" : " no-times"}${layout.showAgents ? "" : " hide-agents"}`}
      data-zoom-pane="sidebar"
    >
      <SidebarResizeHandle
        width={layout.width}
        setWidthLive={setWidthLive}
        onCommit={(w) => updateLayout({ width: w })}
      />
      {/* Absolutely positioned rather than a flex sibling: the sidebar keeps
          its existing single-column structure (and every modal/portal already
          hanging off it) and just gains a gutter. */}
      {railMode && (
        <ProjectRail
          quickThreads={quickThreads}
          quickOn={quickView}
          workspaces={visibleWorkspaces}
          hidden={hiddenWorkspaces}
          threadsByWorkspace={threadsByWorkspace}
          shownId={quickView ? null : shownWorkspaceId}
          onPickQuick={() => switchView("quick")}
          onPick={pickRailProject}
          onMenu={(point, workspace) =>
            setMenu({
              x: point.x,
              y: point.y,
              workspace,
              primary: sectionData.get(workspace.id)?.projects[0],
            })
          }
          onStash={setStashMenu}
          onReorder={commitOrder}
          reorderEnabled={reorderEnabled}
        />
      )}
      <div className="sidebar-brand">
        <button
          ref={filterBtnRef}
          type="button"
          className={`icon-btn sidebar-filter-btn${filterOpen ? " on" : ""}${
            isNonDefaultLayout(layout) ? " active" : ""
          }`}
          aria-label="View options"
          aria-haspopup="dialog"
          aria-expanded={filterOpen}
          title="View options"
          onClick={() => setFilterOpen((v) => !v)}
        >
          <FilterIcon size={18} />
        </button>
        <img className="brand-wordmark" src={wordmarkUrl} alt="ThreadKnot" />
        {/* Same intent either way — put the sidebar away — so it is one
            button. On a phone the drawer is full width, which leaves no
            backdrop to tap, so this is the ONLY way back out of it; do not
            hide it there again. The glyph matches the header hamburger that
            opened the drawer. */}
        <button
          type="button"
          className="icon-btn sidebar-collapse-btn"
          aria-label={state.sidebarOpen ? "Close sidebar" : "Collapse sidebar"}
          title={state.sidebarOpen ? "Close sidebar" : "Collapse sidebar"}
          onClick={() => {
            if (window.matchMedia("(max-width: 767px)").matches) {
              dispatch({ type: "sidebar", open: false });
            } else {
              updateLayout({ collapsed: true });
            }
          }}
        >
          <PanelLeftIcon size={18} />
        </button>
      </div>

      <PeopleRow />

      <div className="sidebar-launch">
        <button
          type="button"
          className="sidebar-action new-chat"
          onClick={startNewChatHere}
        >
          <NotebookPenIcon size={18} />
          <span>New chat</span>
        </button>
        <button
          type="button"
          className="sidebar-action search-open"
          onClick={() => setSearchOpen(true)}
        >
          <SearchIcon size={18} />
          <span>Search</span>
        </button>
        {!soloId && !agentsView && !quickView && (
          <button type="button" className="sidebar-action" onClick={onAddProject}>
            <FolderPlusIcon size={18} />
            <span>Add workspace</span>
          </button>
        )}
        {state.projects.some(
          (p) => p.id !== HERMES_HOME_PROJECT_ID && !isQuickHomeProjectId(p.id),
        ) && (
          <button type="button" className="sidebar-action" onClick={onOpenSchedules}>
            <ClockIcon size={18} />
            <span>Scheduled runs</span>
            {state.schedules.filter((s) => s.enabled).length > 0 && (
              <span className="usermenu-count">
                {state.schedules.filter((s) => s.enabled).length}
              </span>
            )}
          </button>
        )}
        {!soloId && !quickView && (
          <>
            <button
              type="button"
              className="sidebar-action"
              onClick={() => switchView("quick")}
            >
              <CompassIcon size={18} />
              <span>Quick threads</span>
            </button>
          </>
        )}
        {showHermesAgents() && !soloId && !agentsView && (
          <button
            type="button"
            className="sidebar-action"
            onClick={() => switchView("agents")}
          >
            <AgentMark agent="hermes" size={18} />
            <span>Hermes agents</span>
            {hermesAttention > 0 && <span className="usermenu-dot" />}
          </button>
        )}
        {(agentsView || quickView) && (
          <button
            type="button"
            className="sidebar-action"
            onClick={() => switchView("fleet")}
          >
            <FolderIcon size={18} />
            <span>Workspaces</span>
          </button>
        )}
      </div>
      {filterOpen && (
        <SidebarViewPopover
          anchorRef={filterBtnRef}
          layout={layout}
          hiddenCount={hiddenWorkspaces.length}
          update={updateLayout}
          onClose={() => setFilterOpen(false)}
        />
      )}
      {searchOpen && <ThreadSearchModal onClose={() => setSearchOpen(false)} />}

      <div className="sidebar-scroll" ref={scrollRef} onScroll={onSidebarScroll}>
        {quickView && (
          <QuickChatsSection
            threads={matchingQuickThreads}
            forceOpen={!!filter}
            autoSettleDays={sidebarPrefs.autoSettleDays}
            now={now}
            view={layout.view}
          />
        )}
        {!quickView && !agentsView && !!filter && matchingQuickThreads.length > 0 && (
          <QuickChatsSection
            threads={matchingQuickThreads}
            forceOpen
            autoSettleDays={sidebarPrefs.autoSettleDays}
            now={now}
            view={layout.view}
          />
        )}
        {agentsView && (
          <HermesSection
            filter={filter}
            contentMatches={contentMatches}
            searchingContent={searchingContent}
            autoSettleDays={sidebarPrefs.autoSettleDays}
            now={now}
            view={layout.view}
          />
        )}
        {!quickView && !agentsView && !soloId && sections.length === 0 && (
          <div className="sidebar-empty">
            <p>No projects in the fleet yet.</p>
          </div>
        )}
        {!quickView && !agentsView && soloId && visibleWorkspaces.length === 0 && !filter && (
          <div className="sidebar-empty">
            <p>This project was removed from the fleet.</p>
          </div>
        )}
        {!quickView &&
          !agentsView &&
          filter &&
          visibleWorkspaces.length === 0 &&
          matchingQuickThreads.length === 0 && (
          <div className="sidebar-empty">
            <p>
              {searchingContent ? "Searching thread content…" : "No matching threads."}
            </p>
          </div>
        )}
        {/* Rendered for exactly the layouts that pass `hideHeader` below: this
            bar IS the header there, so it has to carry the header's actions
            (new thread, workspace menu) or they are reachable from nowhere. */}
        {!quickView && !agentsView &&
          (projectLayout === "picker" || projectLayout === "rail") &&
          pickedWorkspace && (
            <div className="project-picker">
              {renamingId === pickedWorkspace.id ? (
                <PickerRenameInput
                  workspace={pickedWorkspace}
                  onDone={() => setRenamingId(null)}
                />
              ) : (
              <button
                type="button"
                className="project-picker-btn"
                // Clicking the name always opens the workspace switcher — the
                // rail isn't there to switch from on a phone. Actions stay on
                // right-click / long-press.
                aria-haspopup="menu"
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  setPickerMenu({ x: r.left, y: r.bottom + 4 });
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setMenu({
                    x: e.clientX,
                    y: e.clientY,
                    workspace: pickedWorkspace,
                    primary: sectionData.get(pickedWorkspace.id)?.projects[0],
                  });
                }}
                >
                <span className="project-picker-name">
                  {pickedWorkspace.name}
                </span>
                {/* Same label as the list layout's header: which machine this
                    workspace lives on, when it is not this one. */}
                {(() => {
                  const guest = workspaceServer(state, pickedWorkspace);
                  return guest ? (
                    <span className="project-server-chip" title={`on ${guest.name}`}>
                      {guest.name}
                    </span>
                  ) : null;
                })()}
                <span className="project-picker-count">
                  {sectionData.get(pickedWorkspace.id)?.threads.length ?? 0}
                </span>
              </button>
              )}
            </div>
          )}
        {pickerMenu && (
          <ContextMenu
            x={pickerMenu.x}
            y={pickerMenu.y}
            title="Switch workspace"
            onClose={() => setPickerMenu(null)}
            items={[
              ...visibleWorkspaces.map((workspace) => ({
                label: workspace.name,
                icon:
                  workspace.id === pickedWorkspace?.id ? (
                    <CheckIcon size={15} />
                  ) : (
                    <FolderIcon size={15} />
                  ),
                onSelect: () => setPickedId(workspace.id),
              })),
              ...(pickedWorkspace
                ? [
                    {
                      label: "Add folder",
                      icon: <FolderPlusIcon size={15} />,
                      dividerBefore: true,
                      onSelect: () => setFolderDialog({ workspaceId: pickedWorkspace.id }),
                    },
                  ]
                : []),
            ]}
          />
        )}
        {!quickView && !agentsView &&
          laidOutWorkspaces.map((w) => {
            const data = sectionData.get(w.id) ?? {
              projects: [],
              members: [],
              threads: [],
              lastActivity: undefined,
            };
            const threads = filter
              ? w.name.toLowerCase().includes(filter)
                ? data.threads
                : data.threads.filter((t) =>
                    threadMatches(t, filter, contentMatches),
                  )
              : data.threads;
            return (
              <WorkspaceSection
                key={w.id}
                workspace={w}
                primary={data.projects[0]}
                members={data.members}
                threads={threads}
                lastActivity={data.lastActivity}
                showLocalBadge={
                  layout.pinLocal && data.members.some((m) => m.isLocal)
                }
                view={layout.view}
                autoSettleDays={sidebarPrefs.autoSettleDays}
                now={now}
                collapsed={
                  projectLayout === "accordion"
                    ? w.id !== shownWorkspaceId
                    : !!state.collapsed[w.id]
                }
                hideHeader={projectLayout === "picker" || projectLayout === "rail"}
                reorder={
                  reorderEnabled &&
                  (projectLayout === "sections" || projectLayout === "accordion")
                    ? {
                        handle: sectionDrag.handleProps(w.id),
                        dragging: sectionDrag.draggingId === w.id,
                        drop:
                          sectionDrag.dropId === w.id
                            ? sectionDrag.dropSide
                            : undefined,
                      }
                    : undefined
                }
                pageSize={
                  projectLayout === "sections" ? THREAD_PAGE_SIZE : SOLO_PROJECT_PAGE_SIZE
                }
                onToggle={
                  projectLayout === "accordion"
                    ? () => setPickedId(w.id === shownWorkspaceId ? "" : w.id)
                    : undefined
                }
                forceOpen={!!filter}
                renaming={renamingId === w.id}
                onRenameDone={() => setRenamingId(null)}
                chatFolders={layout.chatFolders.filter(
                  (folder) => folder.workspaceId === w.id,
                )}
                chatFolderAssignments={layout.chatFolderAssignments}
                onMoveChatToFolder={moveChatToFolder}
                onAddChatFolder={(threadId) =>
                  setFolderDialog({ workspaceId: w.id, moveThreadId: threadId })
                }
                onMenu={(point, workspace) =>
                  setMenu({
                    x: point.x,
                    y: point.y,
                    workspace,
                    primary: sectionData.get(workspace.id)?.projects[0],
                  })
                }
                onNewThread={(e) =>
                  startNewThread(w.id, { x: e.clientX, y: e.clientY })
                }
                onReorderThreads={commitThreadOrder}
                threadOrder={layout.threadOrder}
                sidebarScrollRef={scrollRef}
              />
            );
          })}
      </div>

      {/* Footer: usage above a lone Settings button, both styled like the launch
          buttons. They sit at the bottom because the scroll above is flex:1. */}
      <UsageMeter />
      <button
        type="button"
        className="sidebar-action sidebar-settings"
        onClick={() => setShowSettings(true)}
      >
        <GearIcon size={18} />
        <span>Settings</span>
        {updateReady && <span className="usermenu-dot" title={updateHint} />}
      </button>
      {showSettings && (
        <SettingsScreen onClose={() => setShowSettings(false)} />
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          // Named because the rail opens this on an avatar-only tile, where the
          // items alone never say which project they act on.
          title={menu.workspace.name}
          onClose={() => setMenu(null)}
          items={[
            ...(!state.solo && menu.primary
              ? [
                  {
                    label: state.isTauri
                      ? "Break out into its own window"
                      : "Open in a new tab",
                    icon: <PopoutIcon size={13} />,
                    onSelect: () => {
                      const p = menu.primary;
                      if (p) void openProjectWindow(p);
                    },
                  },
                ]
              : []),
            ...(menu.primary
              ? [
                  {
                    label: "New thread",
                    icon: <PlusIcon size={13} />,
                    onSelect: () => {
                      const p = menu.primary;
                      if (!p) return;
                      // Follow the draft: the rail can open this menu on a
                      // project it is not showing, and leaving the highlight
                      // (and the chat list) on the old one while the pane
                      // filled with the new draft is the exact mismatch
                      // `pickRailProject` exists to avoid.
                      setPickedId(menu.workspace.id);
                      actions.openDraft(p.id);
                    },
                  },
                ]
              : []),
            {
              // Reads as plain "notify me / don't", whichever way the scope
              // makes the underlying list behave.
              label: !notifyPrefs.enabled || notifyPrefs.scope === "none"
                ? "Notifications are off on this device"
                : isWorkspaceSubscribed(notifyPrefs, menu.workspace.id)
                  ? "Mute notifications on this device"
                  : "Notify me about this workspace",
              icon: (
                <BellIcon
                  size={13}
                  muted={!isWorkspaceSubscribed(notifyPrefs, menu.workspace.id)}
                />
              ),
              onSelect: () => {
                if (!notifyPrefs.enabled || notifyPrefs.scope === "none") return;
                toggleWorkspaceNotify(menu.workspace.id);
              },
            },
            {
              label: menu.workspace.favorite ? "Unfavorite workspace" : "Favorite workspace",
              icon: <StarIcon size={13} filled={!!menu.workspace.favorite} />,
              onSelect: () =>
                void actions.setWorkspaceFavorite(
                  menu.workspace.id,
                  !menu.workspace.favorite,
                ),
            },
            // Not offered in a solo window: it is already one project, and it
            // has no rail or view popover to bring anything back from.
            ...(!state.solo
              ? [
                  {
                    label: menu.workspace.hidden
                      ? "Unhide workspace"
                      : "Hide workspace",
                    icon: <EyeIcon size={13} off={!menu.workspace.hidden} />,
                    onSelect: () =>
                      menu.workspace.hidden
                        ? unhideWorkspace(menu.workspace.id)
                        : hideWorkspace(menu.workspace.id),
                  },
                ]
              : []),
            {
              // Deliberately not `danger`: this is a shelf, not a delete. The
              // wording says what it costs to undo, because the flag rides the
              // mesh replica — put a project away here and it is away on every
              // machine you have paired, phone included.
              label: menu.workspace.hidden
                ? "Show project again"
                : "Hide project everywhere",
              icon: <EyeIcon size={13} off={!menu.workspace.hidden} />,
              onSelect: () =>
                menu.workspace.hidden
                  ? unhideWorkspace(menu.workspace.id)
                  : hideWorkspace(menu.workspace.id),
            },
            // Everything below edits the workspace RECORD, which for a guest
            // link lives on somebody else's machine. None of these kinds
            // route, so offering them would produce "unknown workspace" and
            // nothing else. Hiding is different and stays above: it is a
            // sidebar opinion, not an edit to their record.
            ...(workspaceServer(state, menu.workspace)
              ? []
              : [
            {
              label: "Rename workspace",
              icon: <PencilIcon size={13} />,
              onSelect: () => {
                // The picker/rail bar is the only place those layouts can host
                // the inline renamer, and it can only host the project it is
                // showing — so renaming a tile from the rail switches to it.
                if (projectLayout === "picker" || projectLayout === "rail") {
                  setPickedId(menu.workspace.id);
                }
                setRenamingId(menu.workspace.id);
              },
            },
            {
              label: menu.workspace.image
                ? "Change workspace image…"
                : "Add workspace image…",
              icon: <PencilIcon size={13} />,
              onSelect: () =>
                chooseSidebarImage((image) =>
                  actions.setWorkspaceImage(menu.workspace.id, image),
                ),
            },
            ...(menu.workspace.image
              ? [
                  {
                    label: "Remove workspace image",
                    icon: <TrashIcon size={13} />,
                    danger: true,
                    onSelect: () =>
                      void actions.setWorkspaceImage(menu.workspace.id),
                  },
                ]
              : []),
            ...(!state.solo
              ? [
                  {
                    label: "Machines & folders…",
                    icon: <FolderIcon size={13} />,
                    onSelect: () => setRootsModal(menu.workspace),
                  },
                ]
              : []),
              ]),
          ]}
        />
      )}

      {folderDialog && (
        <ChatFolderDialog
          folders={layout.chatFolders.filter(
            (folder) => folder.workspaceId === folderDialog.workspaceId,
          )}
          onClose={() => setFolderDialog(null)}
          onCreate={(name) =>
            createChatFolder(
              name,
              folderDialog.workspaceId,
              folderDialog.moveThreadId,
            )
          }
        />
      )}

      {/* The rail's stash, opened from the tile at the foot of the column.
          The menu restores the names that the compact rail intentionally keeps
          out of its resting state,
          and one click both unhides it and takes you there. The trailing
          "unhide all" only appears when there is more than one to save, since
          for a single project it would just duplicate the row above it. */}
      {stashMenu && hiddenWorkspaces.length > 0 && (
        <ContextMenu
          x={stashMenu.x}
          y={stashMenu.y}
          title={`Hidden (${hiddenWorkspaces.length})`}
          onClose={() => setStashMenu(null)}
          items={[
            ...hiddenWorkspaces.map((w) => ({
              label: w.name,
              icon: (
                <MachineAvatar
                  image={w.image}
                  color={projectAccent(w.id)}
                  name={w.name}
                  size={16}
                  preview={false}
                />
              ),
              onSelect: () => unhideWorkspace(w.id),
            })),
            ...(hiddenWorkspaces.length > 1
              ? [
                  {
                    label: "Unhide all",
                    icon: <EyeIcon size={13} />,
                    onSelect: () => {
                      for (const w of hiddenWorkspaces) {
                        void actions.setWorkspaceHidden(w.id, false);
                      }
                    },
                  },
                ]
              : []),
          ]}
        />
      )}

      {newThreadMenu &&
        // Resolve the workspace's roots FRESH on every render, not from a
        // snapshot taken when the menu opened, so a peer going offline (or
        // coming back) while the menu is open flips its roots live. If the
        // workspace disappears while the menu is open, render nothing.
        (() => {
          const members = sectionData.get(newThreadMenu.workspaceId)?.members;
          if (!members) return null;
          return (
            <ContextMenu
              x={newThreadMenu.x}
              y={newThreadMenu.y}
              onClose={() => setNewThreadMenu(null)}
              items={[
                // Existing roots: click to draft straight into one.
                ...members.map((m) => ({
                  label: `${m.rootLabel} · ${m.machineLabel}${m.online ? "" : " (offline)"}`,
                  icon: <MachineAvatar {...machineLook(state, m.machineId)} size={16} />,
                  disabled: !m.online,
                  onSelect: () => actions.openDraft(m.projectId, m.machineId || undefined),
                })),
                // "New folder on <machine>...": add a project folder to the
                // workspace on any machine (this one first, then each peer), then
                // draft there. Offered for machines already in the workspace AND
                // ones not on it yet; offline peers stay listed but disabled.
                //
                // Servers are deliberately NOT offered here. This attaches a
                // root to OUR workspace record and replicates it to our peers;
                // a server's folders belong to its own catalog and must never
                // land in a record we hand out. Creating a whole workspace over
                // there is the supported move — "Add workspace" offers it.
                ...[
                  {
                    machineId: localId,
                    label: state.hello?.friendlyName ?? "this machine",
                    online: true,
                  },
                  ...state.peers.map((p) => ({
                    machineId: p.machineId,
                    label: p.name,
                    online: !!p.online,
                  })),
                ].map((mc) => ({
                  label: `New folder on ${mc.label}...${mc.online ? "" : " (offline)"}`,
                  icon: <MachineAvatar {...machineLook(state, mc.machineId)} size={16} />,
                  disabled: !mc.online,
                  onSelect: () =>
                    setSetupFolder({
                      workspaceId: newThreadMenu.workspaceId,
                      machineId: mc.machineId,
                      machineLabel: mc.label,
                    }),
                })),
              ]}
            />
          );
        })()}

      {setupFolder && (
        <DirPicker
          title={`Pick the folder on ${setupFolder.machineLabel}`}
          confirmLabel="Use this folder"
          // Only proxy the browse to a peer; browse THIS machine locally.
          machineId={setupFolder.machineId !== localId ? setupFolder.machineId : undefined}
          onClose={() => setSetupFolder(null)}
          onPick={(path) => {
            const target = setupFolder;
            setSetupFolder(null);
            void actions
              // attachRoot works whether or not the machine is already a member
              // (it just adds another root); pass a machineId to openDraft only
              // for a peer, so a local folder drafts here, not as remote.
              .attachRoot(target.workspaceId, target.machineId, path)
              .then((project) =>
                actions.openDraft(
                  project.id,
                  target.machineId !== localId ? target.machineId : undefined,
                ),
              )
              .catch(() => undefined);
          }}
        />
      )}

      {rootsModal && (
        <WorkspaceRootsModal
          workspace={
            state.workspaces.find((w) => w.id === rootsModal.id) ?? rootsModal
          }
          members={sectionData.get(rootsModal.id)?.members ?? []}
          onClose={() => setRootsModal(null)}
        />
      )}
    </aside>
      {/* Desktop-only re-open handle: the collapse toggle rides inside the
          sidebar, so once it animates to zero width we need an affordance that
          lives outside it. Hidden on phones (they use the hamburger drawer). */}
      {layout.collapsed && (
        <button
          type="button"
          className="sidebar-reveal"
          aria-label="Show sidebar"
          title="Show sidebar"
          onClick={() => updateLayout({ collapsed: false })}
        >
          <PanelLeftIcon size={17} />
        </button>
      )}
    </>
  );
});
