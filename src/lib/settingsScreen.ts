// Opening Settings from anywhere.
//
// The Settings screen is mounted by the Sidebar, which owns the state that
// shows it. The thread header needs to open it too, and the two are not in a
// parent/child relationship — so the header asks for it with a window event,
// the same way the composer asks for a workspace to be created. Lighter than
// lifting the state into the store for one boolean that nothing else reads.

export const OPEN_SETTINGS_EVENT = "threadknot:open-settings";

export function openSettings() {
  window.dispatchEvent(new Event(OPEN_SETTINGS_EVENT));
}
