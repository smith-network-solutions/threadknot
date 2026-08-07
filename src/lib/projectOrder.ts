// How the sidebar's manual project order behaves. The order itself lives in
// the sidebar layout prefs (`SidebarLayout.workspaceOrder` in sidebarLayout.ts)
// — one place, so the list cannot sort by one order while a drag writes
// another. These are the two pure rules that act on it.

/**
 * Put `items` in the stored order. Anything the order has never seen — a
 * project added since the last drag, or one that arrived from a peer — keeps
 * its incoming relative order and lands after the placed ones, so a new
 * project appears at the bottom of the rail instead of shuffling the list.
 */
export function applyProjectOrder<T extends { id: string }>(
  items: readonly T[],
  order: readonly string[],
): T[] {
  if (order.length === 0) return [...items];
  const rank = new Map(order.map((id, i) => [id, i]));
  return items
    .map((item, i) => ({ item, i, rank: rank.get(item.id) ?? Infinity }))
    .sort((a, b) => (a.rank === b.rank ? a.i - b.i : a.rank - b.rank))
    .map((entry) => entry.item);
}

/**
 * Fold a freshly dragged order into what was stored. Ids that are no longer on
 * screen (a machine's workspaces before its replica loads, a project open in
 * another window) are kept rather than pruned — dropping them would silently
 * reset their position the first time you dragged anything while they were
 * away — but they park at the end, since a drag can't say where among the
 * visible ones they belong.
 */
export function mergeProjectOrder(
  next: readonly string[],
  stored: readonly string[],
): string[] {
  const shown = new Set(next);
  return [...next, ...stored.filter((id) => !shown.has(id))];
}
