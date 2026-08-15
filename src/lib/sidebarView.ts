/**
 * Which list the sidebar is showing: the workspace fleet, the folderless Quick
 * Threads home, or the dedicated Hermes-agents view. Persisted so the choice
 * survives restarts.
 *
 * This lives outside React because it is no longer only the sidebar's business.
 * The composer needs it too: typing into a chat you reached through the Hermes
 * view means "hand this back to its agent", and the same keystrokes in the same
 * chat reached through its workspace mean nothing of the sort. Same shape as
 * the notify prefs — a snapshot plus a listener set, read with
 * `useSyncExternalStore`.
 */

export type SidebarView = "fleet" | "agents" | "quick";

const LS_KEY = "threadknot.sidebarView";

const listeners = new Set<() => void>();

function read(): SidebarView {
  try {
    const stored = localStorage.getItem(LS_KEY);
    if (stored === "quick" || stored === "agents") return stored;
  } catch {
    // Locked-down storage: the fleet is the right thing to fall back to.
  }
  return "fleet";
}

let current: SidebarView = read();

/** Snapshot for `useSyncExternalStore` — must be referentially stable between
 *  writes, so the value is cached rather than re-read from storage. */
export function getSidebarView(): SidebarView {
  return current;
}

export function setSidebarView(view: SidebarView): void {
  if (view === current) return;
  current = view;
  try {
    localStorage.setItem(LS_KEY, view);
  } catch {
    // Persistence is a convenience; the switch still works for this run.
  }
  for (const fn of listeners) fn();
}

export function subscribeSidebarView(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
