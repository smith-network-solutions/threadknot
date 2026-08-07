// Reliable window-focus tracking.
//
// `document.hasFocus()` is unreliable in the Tauri WebKitGTK webview on
// Wayland — it can report `true` even when the window is blurred, which
// suppressed the "unfocused → system notification" path (you'd get the toast
// + chime but no OS notification). In Tauri we instead trust the compositor
// via the native focus events; in a plain browser the DOM signals are fine.

let windowFocused = typeof document !== "undefined" ? document.hasFocus() : true;
let started = false;

/** Best-known focus state of the app window (updated by startFocusTracking). */
export function isWindowFocused(): boolean {
  return windowFocused;
}

/**
 * Begin tracking real window focus. Returns a cleanup function.
 * Tauri: authoritative native focus events. Browser: DOM focus/blur/visibility.
 */
export function startFocusTracking(isTauri: boolean): () => void {
  if (started) return () => undefined;
  started = true;
  const cleanups: Array<() => void> = [];

  if (isTauri) {
    void (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const win = getCurrentWindow();
        windowFocused = await win.isFocused();
        const unlisten = await win.onFocusChanged(({ payload }) => {
          windowFocused = payload;
        });
        cleanups.push(unlisten);
      } catch {
        // Native focus unavailable — fall back to DOM signals below.
        attachDomSignals(cleanups);
      }
    })();
  } else {
    attachDomSignals(cleanups);
  }

  return () => {
    for (const c of cleanups) c();
    cleanups.length = 0;
    started = false;
  };
}

function attachDomSignals(cleanups: Array<() => void>): void {
  const onFocus = () => {
    windowFocused = true;
  };
  const onBlur = () => {
    windowFocused = false;
  };
  const onVis = () => {
    if (document.hidden) windowFocused = false;
  };
  window.addEventListener("focus", onFocus);
  window.addEventListener("blur", onBlur);
  document.addEventListener("visibilitychange", onVis);
  cleanups.push(() => {
    window.removeEventListener("focus", onFocus);
    window.removeEventListener("blur", onBlur);
    document.removeEventListener("visibilitychange", onVis);
  });
}
