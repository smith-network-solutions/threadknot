import { useCallback, useEffect, useRef, useState } from "react";

export interface ChatFolder {
  id: string;
  workspaceId: string;
  name: string;
}

/** Sidebar presentation preferences, persisted whole so a single read/write
 *  keeps the four knobs in sync. Consumed by the Sidebar and (later) the card
 *  view and hover cards built on top of it. */
export interface SidebarLayout {
  /** Slim list rows, denser compact rows, or the richer card grid. */
  view: "list" | "compact" | "cards";
  /** Bigger, semibold workspace names for readability. */
  bigNames: boolean;
  /** Float workspaces that live on this machine to the top of the fleet. */
  pinLocal: boolean;
  /** Show the relative-time column on thread rows (list + cards). Compact view
   *  always hides it regardless (the time still lives in the hover card). */
  showTimes: boolean;
  /** Reveal workspaces stashed with "Hide project" alongside the rest, marked
   *  rather than removed, so a batch can be brought back in place. This is a
   *  per-device view toggle; what is hidden is on the workspace record and
   *  syncs mesh-wide. */
  showHidden: boolean;
  /** Sidebar width in px, mirrored onto the --sidebar-w CSS variable. */
  width: number;
  /** Collapsed (animated to zero width) on desktop. Ignored on mobile, where
   *  the sidebar is a slide-over drawer toggled by the hamburger instead. */
  collapsed: boolean;
  /** Manual workspace order by id. Listed ids sort first in this order; ids
   *  absent from the list fall back to their activity order after them; ids no
   *  longer in state are ignored and pruned on the next reorder write. Excluded
   *  from the filter tint - a drag order is not a "filter". */
  workspaceOrder: string[];
  /** User-made chat groups inside workspaces. Like manual order, these are
   *  presentation preferences for this device rather than project metadata. */
  chatFolders: ChatFolder[];
  /** Thread id -> folder id. Missing means the chat stays at workspace root. */
  chatFolderAssignments: Record<string, string>;
}

export const SIDEBAR_WIDTH_DEFAULT = 292;
export const SIDEBAR_WIDTH_MIN = 240;
export const SIDEBAR_WIDTH_MAX = 460;

const LS_SIDEBAR_LAYOUT = "threadknot.sidebarLayout";

const DEFAULTS: SidebarLayout = {
  view: "list",
  bigNames: false,
  pinLocal: true,
  showTimes: true,
  showHidden: false,
  width: SIDEBAR_WIDTH_DEFAULT,
  collapsed: false,
  workspaceOrder: [],
  chatFolders: [],
  chatFolderAssignments: {},
};

/** True when nothing is at its default: drives the filter button's tint.
 *  workspaceOrder is deliberately excluded - a drag order is not a "filter". */
export function isNonDefaultLayout(l: SidebarLayout): boolean {
  return (
    l.view !== DEFAULTS.view ||
    l.bigNames !== DEFAULTS.bigNames ||
    l.pinLocal !== DEFAULTS.pinLocal ||
    l.showTimes !== DEFAULTS.showTimes ||
    l.showHidden !== DEFAULTS.showHidden
  );
}

/** Read the persisted layout, tolerating missing/malformed JSON and partial
 *  shapes (any absent field falls back to its default). */
function loadLayout(): SidebarLayout {
  try {
    const raw = localStorage.getItem(LS_SIDEBAR_LAYOUT);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<SidebarLayout>;
    return {
      // Unknown values fall back to "list".
      view:
        parsed.view === "cards"
          ? "cards"
          : parsed.view === "compact"
            ? "compact"
            : "list",
      bigNames: parsed.bigNames === true,
      // pinLocal defaults to true, so only an explicit false turns it off.
      pinLocal: parsed.pinLocal !== false,
      // showTimes defaults to true, so only an explicit false turns it off.
      showTimes: parsed.showTimes !== false,
      showHidden: parsed.showHidden === true,
      width: clampWidth(
        typeof parsed.width === "number" ? parsed.width : SIDEBAR_WIDTH_DEFAULT,
      ),
      collapsed: parsed.collapsed === true,
      workspaceOrder: Array.isArray(parsed.workspaceOrder)
        ? parsed.workspaceOrder.filter((x): x is string => typeof x === "string")
        : [],
      chatFolders: Array.isArray(parsed.chatFolders)
        ? parsed.chatFolders.filter(
            (folder): folder is ChatFolder =>
              !!folder &&
              typeof folder === "object" &&
              typeof (folder as ChatFolder).id === "string" &&
              typeof (folder as ChatFolder).workspaceId === "string" &&
              typeof (folder as ChatFolder).name === "string" &&
              (folder as ChatFolder).name.trim().length > 0,
          )
        : [],
      chatFolderAssignments:
        parsed.chatFolderAssignments &&
        typeof parsed.chatFolderAssignments === "object" &&
        !Array.isArray(parsed.chatFolderAssignments)
          ? Object.fromEntries(
              Object.entries(parsed.chatFolderAssignments).filter(
                (entry): entry is [string, string] =>
                  typeof entry[0] === "string" && typeof entry[1] === "string",
              ),
            )
          : {},
    };
  } catch {
    return { ...DEFAULTS };
  }
}

export function clampWidth(w: number): number {
  if (!Number.isFinite(w)) return SIDEBAR_WIDTH_DEFAULT;
  return Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, Math.round(w)));
}

export interface UseSidebarLayout {
  layout: SidebarLayout;
  /** Merge a patch into the layout and persist the whole object. */
  update: (patch: Partial<SidebarLayout>) => void;
  /** Set the width in state + CSS var WITHOUT persisting, for live drag. */
  setWidthLive: (width: number) => void;
}

/** Instantiated once in Sidebar and threaded down. Keeps the --sidebar-w CSS
 *  variable in lockstep with the width so every consumer of the var (e.g. the
 *  pop-out zone) follows resizes without reading our state. */
export function useSidebarLayout(): UseSidebarLayout {
  const [layout, setLayout] = useState<SidebarLayout>(loadLayout);
  // Latest layout for the live-width path, which sets the var directly and
  // must not stomp other fields when it later persists.
  const ref = useRef(layout);
  ref.current = layout;

  // Mirror the width onto the document so all var(--sidebar-w) users follow.
  // Runs on mount and on every width change.
  useEffect(() => {
    document.documentElement.style.setProperty("--sidebar-w", `${layout.width}px`);
  }, [layout.width]);

  const persist = useCallback((next: SidebarLayout) => {
    try {
      localStorage.setItem(LS_SIDEBAR_LAYOUT, JSON.stringify(next));
    } catch {
      // Persistence is a convenience only.
    }
  }, []);

  const update = useCallback(
    (patch: Partial<SidebarLayout>) => {
      setLayout((prev) => {
        const next = { ...prev, ...patch };
        if (patch.width !== undefined) next.width = clampWidth(next.width);
        persist(next);
        return next;
      });
    },
    [persist],
  );

  // Live drag: update state + var but skip localStorage on every pointer move;
  // the caller persists once on pointer-up via update({ width }).
  const setWidthLive = useCallback((width: number) => {
    const w = clampWidth(width);
    setLayout((prev) => (prev.width === w ? prev : { ...prev, width: w }));
  }, []);

  return { layout, update, setWidthLive };
}
