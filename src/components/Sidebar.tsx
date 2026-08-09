import {
  useCallback,
  useEffect,
  useLayoutEffect,
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
import { HERMES_HOME_PROJECT_ID, isQuickHomeProjectId } from "../lib/protocol";
import { SHOW_HERMES_AGENTS } from "../lib/agentVisibility";
import { timeAgo } from "../lib/format";
import {
  findThread,
  hermesAttentionThreads,
  projectActivity,
  threadNeedsAttention,
  threadSettled,
  useStore,
  type AppState,
  type ProjectActivity,
} from "../state/store";
import { SettingsScreen } from "./SettingsPopover";
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
import { applyProjectOrder, mergeProjectOrder } from "../lib/projectOrder";
import { useReorderDrag, type ReorderHandleProps } from "../lib/reorder";
import {
  useSidebarLayout,
  isNonDefaultLayout,
  SIDEBAR_WIDTH_DEFAULT,
  type SidebarLayout,
} from "../lib/sidebarLayout";
import {
  AgentMark,
  ArchiveIcon,
  BellIcon,
  CheckIcon,
  ChevronIcon,
  ClockIcon,
  EyeIcon,
  FilterIcon,
  FolderIcon,
  GearIcon,
  GripIcon,
  MoreIcon,
  PencilIcon,
  PlusIcon,
  PopoutIcon,
  SearchIcon,
  StarIcon,
  TrashIcon,
  UndoIcon,
  XIcon,
} from "./icons";

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

/** The parked tail of a section: one collapsed line by default, so history
 *  costs a row of sidebar instead of N. While a search is running the whole
 *  shelf is flattened open — a chat you cannot find is worse than a chat you
 *  have to un-settle. */
function SettledShelf({
  threads,
  forceOpen,
  view,
}: {
  threads: Thread[];
  forceOpen: boolean;
  /** Sidebar presentation, forwarded to each shelf row. */
  view: SidebarLayout["view"];
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
          <span className="settled-shelf-label">settled</span>
          <span className="settled-shelf-count">{threads.length}</span>
        </button>
      )}
      {shown.map((t) => (
        <ThreadRow
          key={t.id}
          thread={t}
          active={state.activeThreadId === t.id}
          view={view}
          settled
          lit={forceOpen}
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
  const { active, settled } = useSettledSplit(threads, autoSettleDays, now);
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
      <header className="quick-chats-head">
        <span className="quick-chats-mark" aria-hidden>
          <PlusIcon size={15} />
        </span>
        <span className="quick-chats-title">Quick threads</span>
        <ProjectPulse activity={activity} />
        <span className="project-count">{active.length}</span>
        <button
          type="button"
          className="icon-btn quick-new"
          aria-label="New quick thread"
          title="New quick thread"
          onClick={() => actions.openQuickDraft()}
        >
          <PlusIcon size={14} />
        </button>
      </header>
      <div className="quick-chats-rule" aria-hidden />
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
            <ThreadRow
              key={thread.id}
              thread={thread}
              active={state.activeThreadId === thread.id}
              view={view}
            />
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

/** The project rail: every project as an avatar down the left edge, the
 *  selected one filling the rest of the sidebar. Switching costs one tap with
 *  no menu, and — unlike the dropdown picker — the projects you are NOT in
 *  stay on screen, so an unread badge on a project you had forgotten about is
 *  still visible. */
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
            {/* The PROJECT's identity, not the machine's: `.project-head`
                can lean on the machine badge because the name sits beside
                it, but here the badge is all there is.

                Which is exactly why the rail opts back INTO the shared hover
                preview instead of suppressing it: at 38px with no label, seeing
                the image big and reading the name is the whole point of
                hovering a tile. The portaled badge does both, and it escapes
                the rail's scroll clip on its own. */}
            <MachineAvatar
              image={w.image}
              color={projectAccent(w.id)}
              name={w.name}
              size={38}
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

/** Which list the sidebar shows: the workspace fleet, or the dedicated
 *  Hermes-agents view. Persisted so the choice survives restarts. */
type SidebarView = "fleet" | "agents" | "quick";
const LS_SIDEBAR_VIEW = "threadknot.sidebarView";

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
  view,
}: {
  thread: Thread;
  active: boolean;
  /** Rendered as a compact row inside the settled shelf. The row still opens
   *  the chat normally — settling is about attention, not access. */
  settled?: boolean;
  /** Keep a shelf row at full strength. Set while searching: a match that
   *  renders at resting-shelf opacity reads as a disabled non-result. */
  lit?: boolean;
  /** Which sidebar presentation to render. Threaded down from the layout hook
   *  (a fresh useSidebarLayout() here would be a second, out-of-sync copy). */
  view: SidebarLayout["view"];
}) {
  const { state, dispatch, actions } = useStore();
  const needsAttention = !active && threadNeedsAttention(state, thread);
  // Inverted prominence: a chat that is merely BUSY is not your problem yet,
  // so it recedes. Brightness is reserved for rows that want a human —
  // pending approvals and unread finishes — which is what makes a glance at
  // a 5-project sidebar answer "what needs me?" instead of "what exists?".
  const recede = !active && !needsAttention && thread.status === "running";
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
  // Hermes chats wear their gateway's profile photo instead of the brand mark.
  const hermesRec =
    thread.agent === "hermes"
      ? state.hermesAgents.find((a) => a.id === thread.settings.model)
      : undefined;
  const hermesAvatar = hermesRec?.avatar ?? hermesRec?.image;
  // Live presence of the thread's gateway (keyed by settings.model, the
  // registry id). Only meaningful for a registered gateway; an orphaned thread
  // has no record and shows the brand mark with no dot.
  const hermesStatus = hermesRec ? state.hermesStatuses[thread.settings.model] : undefined;
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

  function closeMenu() {
    setMenuAnchor(null);
    setConfirmDelete(false);
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

  if (editing) {
    return (
      <div
        className={`thread-row editing${card ? " thread-card" : ""}${
          active ? " active" : ""
        }${needsAttention ? " has-attention" : ""}`}
      >
        <span
          className={`status-dot st-${thread.status}${needsAttention ? " unread" : ""}`}
          title={needsAttention ? "Unread activity" : undefined}
        />
        <AgentMark agent={thread.agent} size={12} className="thread-row-mark" />
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
  const statusEl = (
    <span
      className={`status-dot st-${thread.status}${needsAttention ? " unread" : ""}`}
      title={needsAttention ? "Unread activity" : undefined}
    />
  );
  const markEl = hermesAvatar ? (
    <span className="hermes-avatar-wrap">
      <span className="thread-row-avatar" {...hermesPreview.hoverProps}>
        <img src={hermesAvatar} alt="" />
      </span>
      <HermesPresenceDot status={hermesStatus} className="sm" />
    </span>
  ) : (
    <AgentMark agent={thread.agent} size={12} className="thread-row-mark" />
  );
  const chipsEl = (
    <>
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
              {settled ? <UndoIcon size={13} /> : <CheckIcon size={13} />}
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
              <StarIcon size={13} filled={!!thread.favorite} />
              <span>{thread.favorite ? "Unfavorite" : "Favorite"}</span>
            </button>
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item"
              onClick={startRename}
            >
              <PencilIcon size={13} />
              <span>Rename</span>
            </button>
            <button
              type="button"
              role="menuitem"
              className="thread-menu-item"
              onClick={() => {
                closeMenu();
                void actions.archiveThread(thread.id, thread.projectId);
              }}
            >
              <ArchiveIcon size={13} />
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
              <TrashIcon size={13} />
              <span>{confirmDelete ? "Delete permanently?" : "Delete"}</span>
            </button>
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
          }${settled && lit ? " lit" : ""}`}
          {...rowGestures}
          {...hover.hoverProps}
        >
          <div className="thread-card-top">
            {statusEl}
            {markEl}
            {hermesPreview.portal}
            <span className="thread-card-title">{thread.title || "Untitled thread"}</span>
          </div>
          <div className="thread-card-meta">
            {chipsEl}
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
          }${recede ? " recede" : ""}`}
          {...rowGestures}
          {...hover.hoverProps}
        >
          <div className="thread-row-main">
            {statusEl}
            {markEl}
            {hermesPreview.portal}
            <span className="thread-row-title">{thread.title || "Untitled thread"}</span>
          </div>
          <div className="thread-row-sub">
            {chipsEl}
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
        }`}
        {...rowGestures}
        {...hover.hoverProps}
      >
        {statusEl}
        {markEl}
        {hermesPreview.portal}
        <span className="thread-row-title">{thread.title || "Untitled thread"}</span>
        {chipsEl}
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
}) {
  const { state, dispatch, actions } = useStore();
  const [visibleThreadCount, setVisibleThreadCount] = useState(pageSize);
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
  const attentionCount = threads.filter((thread) =>
    threadNeedsAttention(state, thread),
  ).length;
  // Reads the WHOLE project, parked chats included: a settled chat that starts
  // working again still belongs on its project's status light.
  const activity = projectActivity(state, threads);
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
            <ChevronIcon size={12} open={open} className="row-chevron" />
            {workspace.image ? (
              <span
                className="sidebar-avatar workspace-avatar has-image"
                {...workspacePreview.hoverProps}
              >
                <img src={workspace.image} alt="" />
              </span>
            ) : (
              // No custom workspace image: badge the workspace with its owning
              // machine's avatar/initials.
              <MachineAvatar
                {...machineLook(state, workspace.members[0]?.machineId)}
                size={22}
              />
            )}
            <span className="project-name">{workspace.name}</span>
            {workspace.favorite && (
              <StarIcon size={11} filled className="project-fav-star" />
            )}
            {/* Only reachable while "show hidden" is on (or a search pulled it
                back): says WHY this row is greyed, so a revealed shelf reads as
                stashed rather than broken. */}
            {workspace.hidden && (
              <EyeIcon size={11} off className="project-hidden-mark" />
            )}
            <ProjectPulse activity={activity} count={attentionCount} />
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
        {members.length > 0 && (
          <button
            className="icon-btn"
            aria-label="New thread"
            title="New thread (pick where)"
            onClick={onNewThread}
          >
            <PlusIcon size={13} />
          </button>
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
        <div className="project-threads">
          {shown.length === 0 && settled.length === 0 && (
            <div className="project-empty">
              {forceOpen ? "no matches" : "no threads yet"}
            </div>
          )}
          {shown.map((t) => (
            <ThreadRow
              key={t.id}
              thread={t}
              active={state.activeThreadId === t.id}
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
          {/* The shelf: collapsed to a single line by default, so parked work
              costs one row of sidebar instead of N. Hidden entirely when
              nothing is parked, and skipped while searching (the matches are
              already flattened into the list above). */}
          <SettledShelf threads={settled} forceOpen={forceOpen} view={view} />
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
          <SettledShelf threads={settled} forceOpen={forceOpen} view={view} />
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

/** The sidebar's dedicated agents view: one group per registry gateway
 *  holding every hermes-agent thread (matched by settings.model — the
 *  registry id), regardless of which project the thread was born in.
 *  Workspace sections exclude these threads, so each chat appears exactly
 *  once. Shown INSTEAD of the workspace list (see `SidebarView`) so agents
 *  and workspaces never compete for sidebar space. */
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
      .filter((t) => t.agent === "hermes")
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
      threads: hermesThreads.filter((t) => t.settings.model === g.id),
    })),
  ];
  const orphaned = hermesThreads.filter(
    (t) => !gateways.some((g) => g.id === t.settings.model),
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

export function Sidebar({
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
  const filterBtnRef = useRef<HTMLButtonElement>(null);
  const [view, setView] = useState<SidebarView>(() => {
    try {
      const stored = localStorage.getItem(LS_SIDEBAR_VIEW);
      if (stored === "quick") return "quick";
      if (SHOW_HERMES_AGENTS && stored === "agents") return "agents";
      return "fleet";
    } catch {
      return "fleet";
    }
  });
  const switchView = (v: SidebarView) => {
    setView(v);
    try {
      localStorage.setItem(LS_SIDEBAR_VIEW, v);
    } catch {
      // Persistence is a convenience only.
    }
    // Keep whatever's on screen relevant to the view you just switched to, so a
    // lingering workspace chat doesn't sit under the agents list (or vice-versa).
    // Keep the pane and destination list in agreement. Quick Threads and Hermes
    // are global homes, so neither should leave a workspace thread stranded
    // beneath its list (or vice versa).
    const active = state.activeThreadId
      ? findThread(state, state.activeThreadId)
      : null;
    const activeProjectId = active?.projectId ?? state.draft?.projectId;
    const activeKind: SidebarView | null = !activeProjectId
      ? null
      : isQuickHomeProjectId(activeProjectId)
        ? "quick"
        : activeProjectId === HERMES_HOME_PROJECT_ID || active?.agent === "hermes"
          ? "agents"
          : "fleet";
    if (v === "agents") {
      // Jump straight to a Hermes chat that wants attention; otherwise leave a
      // Hermes chat (or nothing) in place, and clear only a workspace one — we
      // never auto-open a chat when none is asking for it.
      const needy = hermesAttentionThreads(state);
      if (needy.length > 0) {
        if (needy[0].id !== state.activeThreadId)
          void actions.selectThread(needy[0].id);
      } else if (activeKind !== null && activeKind !== "agents") {
        dispatch({ type: "closeActive" });
      }
    } else if (activeKind !== null && activeKind !== v) {
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
  const agentsView = SHOW_HERMES_AGENTS && view === "agents" && !soloId;
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
  useEffect(() => {
    if (soloId || !isQuickHomeProjectId(openProjectId) || view === "quick") return;
    setView("quick");
    try {
      localStorage.setItem(LS_SIDEBAR_VIEW, "quick");
    } catch {
      // Navigation persistence is a convenience only.
    }
  }, [openProjectId, soloId, view]);
  // Hermes chats asking to be looked at — badges the fleet-view button so a
  // finished turn / pending approval is visible without leaving the workspaces.
  const hermesAttention = useMemo(
    () => (SHOW_HERMES_AGENTS ? hermesAttentionThreads(state).length : 0),
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
    () => new Map(state.projects.map((p) => [p.id, p])),
    [state.projects],
  );

  // Workspaces are the sidebar's top level. Until the workspace list arrives
  // (connect race, or an old server) synthesize one section per uncovered
  // project so the sidebar never blanks.
  const sections = useMemo(() => {
    const covered = new Set(
      state.workspaces.flatMap((w) => w.members.map((m) => m.projectId)),
    );
    const synthetic: Workspace[] = state.projects
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
    return [...state.workspaces, ...synthetic];
  }, [state.workspaces, state.projects]);

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
            : (peer?.name ?? "remote machine"),
          isLocal,
          online: isLocal || !!peer?.online,
          path: p?.path ?? m.path ?? "",
        };
      });
      const projects = members
        .filter((m) => m.isLocal)
        .map((m) => projectById.get(m.projectId))
        .filter((p): p is Project => !!p);
      const threads = members
        .flatMap((m) => state.threads[m.projectId] ?? [])
        // Hermes chats live in the dedicated section above the workspaces.
        .filter((t) => t.agent !== "hermes")
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
      map.set(w.id, { projects, members, threads, lastActivity });
    }
    return map;
  }, [sections, soloId, projectById, state.threads, state.peers, state.hello]);

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
  /** Where the rail's stash tile was clicked; non-null renders the bring-back
   *  menu. Portaled from here with the sidebar's other menus. */
  const [stashMenu, setStashMenu] = useState<MenuPoint | null>(null);
  const activeWorkspaceId = useMemo(() => {
    const active = state.activeThreadId
      ? findThread(state, state.activeThreadId)
      : null;
    if (!active) return null;
    for (const [id, data] of sectionData) {
      if (data.threads.some((t) => t.id === active.id)) return id;
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

  return (
    <aside
      className={`sidebar${state.sidebarOpen ? " open" : ""}${
        railMode ? " layout-rail" : ""
      }${layout.view === "cards" ? " cards-view" : ""}${
        layout.view === "compact" ? " compact-view" : ""
      }${layout.bigNames ? " big-names" : ""}${
        layout.showTimes ? "" : " no-times"
      }`}
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
        <img src="/threadknot-logo.png" alt="Threadknot" className="brand-logo" />
        <span className="brand-word">THREADKNOT</span>
        <span
          className={`conn-pip conn-${state.conn}`}
          title={`connection: ${state.conn}`}
        />
      </div>

      <div className="sidebar-search">
        <SearchIcon size={14} className="search-glyph" />
        <input
          type="search"
          placeholder={
            quickView
              ? "Search quick threads…"
              : agentsView
                ? "Search agent threads…"
                : "Search threads…"
          }
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-busy={!!filter && searchingContent}
        />
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
          <FilterIcon size={15} />
        </button>
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
                // The rail already owns switching workspaces, so there the
                // name is a label with a menu rather than a second picker.
                aria-haspopup="menu"
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  const point = { x: r.left, y: r.bottom + 4 };
                  if (projectLayout === "picker") setPickerMenu(point);
                  else
                    setMenu({
                      ...point,
                      workspace: pickedWorkspace,
                      primary: sectionData.get(pickedWorkspace.id)?.projects[0],
                    });
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
                {/* Only the picker needs a badge here. In rail mode the rail
                    already shows this project's own avatar right above, and
                    this one is the MACHINE's — a second, different image for
                    the same row. */}
                {projectLayout === "picker" && (
                  <MachineAvatar
                    {...machineLook(
                      state,
                      pickedWorkspace.members[0]?.machineId,
                    )}
                    size={20}
                  />
                )}
                <span className="project-picker-name">
                  {pickedWorkspace.name}
                </span>
                <ChevronIcon
                  size={12}
                  open={!!pickerMenu}
                  className="row-chevron"
                />
              </button>
              )}
              <ProjectPulse
                activity={projectActivity(
                  state,
                  sectionData.get(pickedWorkspace.id)?.threads ?? [],
                )}
              />
              <span className="project-picker-count">
                {sectionData.get(pickedWorkspace.id)?.threads.length ?? 0}
              </span>
              {(sectionData.get(pickedWorkspace.id)?.members.length ?? 0) >
                0 && (
                <button
                  className="icon-btn"
                  aria-label="New thread"
                  title={
                    (sectionData.get(pickedWorkspace.id)?.members.length ?? 0) >
                    1
                      ? "New thread (pick machine + folder)"
                      : "New thread"
                  }
                  onClick={(e) =>
                    startNewThread(pickedWorkspace.id, {
                      x: e.clientX,
                      y: e.clientY,
                    })
                  }
                >
                  <PlusIcon size={13} />
                </button>
              )}
            </div>
          )}
        {pickerMenu && (
          <ContextMenu
            x={pickerMenu.x}
            y={pickerMenu.y}
            onClose={() => setPickerMenu(null)}
            items={visibleWorkspaces.map((w) => ({
              label: w.name,
              icon: <FolderIcon size={13} />,
              onSelect: () => setPickedId(w.id),
            }))}
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
              />
            );
          })}
      </div>

      <div className="sidebar-actions">
        {!soloId && !agentsView && !quickView && (
          <button className="add-project" onClick={onAddProject}>
            <PlusIcon size={14} />
            <span>add workspace</span>
          </button>
        )}
        {!soloId &&
          !agentsView &&
          !quickView &&
          state.projects.some(
            (p) => p.id !== HERMES_HOME_PROJECT_ID && !isQuickHomeProjectId(p.id),
          ) && (
            <button
              className="add-project sched-entry"
              onClick={onOpenSchedules}
            >
              <ClockIcon size={14} />
              <span>scheduled runs</span>
              {state.schedules.filter((s) => s.enabled).length > 0 && (
                <span className="sched-count">
                  {state.schedules.filter((s) => s.enabled).length}
                </span>
              )}
            </button>
          )}
        {!soloId && !agentsView && !quickView && !railMode && (
          <button className="add-project quick-entry" onClick={() => switchView("quick")}>
            <span className="quick-entry-mark" aria-hidden>
              <PlusIcon size={13} />
            </span>
            <span>quick threads</span>
          </button>
        )}
        {SHOW_HERMES_AGENTS && !soloId && !agentsView && !quickView && (
          <button
            className={`add-project hermes-entry${hermesAttention > 0 ? " has-attention" : ""}`}
            onClick={() => switchView("agents")}
          >
            <AgentMark agent="hermes" size={14} />
            <span>hermes agents</span>
            {hermesAttention > 0 && (
              <span
                className="hermes-attention-dot"
                title={`${hermesAttention} Hermes ${
                  hermesAttention === 1 ? "chat needs" : "chats need"
                } attention`}
              />
            )}
          </button>
        )}
        {(agentsView || quickView) && (
          <button className="add-project" onClick={() => switchView("fleet")}>
            <FolderIcon size={14} />
            <span>workspaces</span>
          </button>
        )}
        <UsageMeter />
      </div>

      <div className="sidebar-foot">
        <button
          className={`icon-btn foot-gear${showSettings ? " on" : ""}${
            updateReady ? " update-pulse" : ""
          }`}
          aria-label={
            updateReady ? "Settings — an update is available" : "Settings"
          }
          title={updateReady ? updateHint : undefined}
          onClick={() => setShowSettings((v) => !v)}
        >
          <GearIcon size={16} />
        </button>
        <VersionBadge />
        {showSettings && (
          <SettingsScreen onClose={() => setShowSettings(false)} />
        )}
      </div>

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
          ]}
        />
      )}

      {/* The rail's stash, opened from the tile at the foot of the column.
          Every hidden project by name and avatar — the rail is avatars-only, so
          a list that says what each one IS is the whole point of the menu —
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
  );
}
