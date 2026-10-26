/**
 * "Most recently used" — a per-device record of what YOU last opened.
 *
 * Deliberately not `updatedAt`. The sidebar used to float workspaces by their
 * newest thread's activity, and it was removed because the list rearranged
 * itself under the cursor exactly when it was busiest: an agent finishing a
 * turn somewhere else slid a different project under the click (see the
 * ordering comment above `visibleWorkspaces` in Sidebar.tsx). Recording the
 * moment you NAVIGATE to something keeps that float honest — the list can only
 * move in response to your own click, never to a background turn. Activity
 * still shows, on the status light and the unread badge; it just moves nothing.
 *
 * Per-device by design, like the rest of the sidebar's presentation prefs: the
 * order your laptop's sidebar is in is not a fact about the project, and
 * syncing it would let a phone you picked up reshuffle the desktop.
 */

export interface RecentUse {
  /** Thread id -> epoch ms you last opened it. */
  threads: Record<string, number>;
  /** Project id -> epoch ms. Stamped alongside the thread, because a chat is
   *  how you usually "use" a workspace and the thread knows its project. */
  projects: Record<string, number>;
  /** Workspace id -> epoch ms, for picks that name a workspace directly (a
   *  rail tile, a section header) and may not land on a thread at all. */
  workspaces: Record<string, number>;
}

const LS_RECENT_USE = "threadknot.recentUse";

/** Fired after every stamp so the sidebar re-sorts without polling. */
export const RECENTUSE_EVENT = "threadknot:recentuse";

/** Entries kept per bucket. Enough that the float still has something to say
 *  about a project you have not touched in a month, small enough that the
 *  record cannot grow without bound on a long-lived install. Pruning drops the
 *  oldest, which are exactly the ones a recency sort cares least about. */
const MAX_ENTRIES = 400;

const EMPTY: RecentUse = { threads: {}, projects: {}, workspaces: {} };

/** Keep only string keys with finite numeric values — a hand-edited or
 *  half-written localStorage entry must not poison the sort with NaN. */
function readBucket(raw: unknown): Record<string, number> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  return Object.fromEntries(
    Object.entries(raw as Record<string, unknown>).filter(
      (entry): entry is [string, number] =>
        typeof entry[0] === "string" &&
        typeof entry[1] === "number" &&
        Number.isFinite(entry[1]),
    ),
  );
}

export function getRecentUse(): RecentUse {
  try {
    const raw = localStorage.getItem(LS_RECENT_USE);
    if (!raw) return { ...EMPTY };
    const parsed = JSON.parse(raw) as Partial<RecentUse>;
    return {
      threads: readBucket(parsed.threads),
      projects: readBucket(parsed.projects),
      workspaces: readBucket(parsed.workspaces),
    };
  } catch {
    return { ...EMPTY };
  }
}

function prune(bucket: Record<string, number>): Record<string, number> {
  const keys = Object.keys(bucket);
  if (keys.length <= MAX_ENTRIES) return bucket;
  return Object.fromEntries(
    keys
      .sort((a, b) => bucket[b] - bucket[a])
      .slice(0, MAX_ENTRIES)
      .map((k) => [k, bucket[k]]),
  );
}

/** Stamp one or more ids as used now. Buckets are merged rather than replaced
 *  so a thread pick can write its thread and its project in one go — and one
 *  write means one event, so the sidebar re-sorts once per navigation. */
export function markUsed(patch: {
  threadId?: string;
  projectId?: string;
  workspaceId?: string;
}): void {
  const { threadId, projectId, workspaceId } = patch;
  if (!threadId && !projectId && !workspaceId) return;
  const now = Date.now();
  const current = getRecentUse();
  const next: RecentUse = {
    threads: threadId
      ? prune({ ...current.threads, [threadId]: now })
      : current.threads,
    projects: projectId
      ? prune({ ...current.projects, [projectId]: now })
      : current.projects,
    workspaces: workspaceId
      ? prune({ ...current.workspaces, [workspaceId]: now })
      : current.workspaces,
  };
  try {
    localStorage.setItem(LS_RECENT_USE, JSON.stringify(next));
  } catch {
    // A full or disabled store costs the float, not the navigation.
  }
  window.dispatchEvent(new CustomEvent<RecentUse>(RECENTUSE_EVENT, { detail: next }));
}

/**
 * Float the items you have opened to the top, newest first, WITHOUT disturbing
 * anything you have not.
 *
 * Items with no stamp keep their incoming relative order and sit below the
 * stamped ones, so turning the toggle on in a fresh install changes nothing
 * until you actually open something, and a workspace you have never visited
 * never jumps the queue. `stampOf` returns 0/undefined for "never opened".
 */
export function floatRecentFirst<T>(
  items: readonly T[],
  stampOf: (item: T) => number | undefined,
): T[] {
  return items
    .map((item, i) => ({ item, i, at: stampOf(item) ?? 0 }))
    .sort((a, b) => (a.at === b.at ? a.i - b.i : b.at - a.at))
    .map((entry) => entry.item);
}
