// Conversation zoom hotkeys, wired once from App: ctrl/cmd + wheel and
// ctrl/cmd + = / - / 0 step the interface zoom through the existing
// appearance store (persisted + broadcast, same as the settings stepper).
// The zoom scales the thread's message feed only, so the band is the plain
// ZOOM_MIN..getEffectiveMaxZoom() (= ZOOM_MAX); stepping at either end is a
// no-op.
// Terminals opt out: ctrl+wheel over a terminal scales that terminal's own
// font (see TerminalInstance); keystrokes inside xterm stay with the pty.
// Mac trackpad pinch arrives as ctrl+wheel, so the same path covers it.

import {
  clamp,
  getAppearance,
  getAppliedZoom,
  getEffectiveMaxZoom,
  setAppearance,
  ZOOM_MIN,
  ZOOM_STEP,
} from "./appearance";

/** deltaY per zoom step: one detented wheel notch in Chromium. Trackpad
 *  pinches stream small fractional deltas, so accumulate up to the notch. */
const WHEEL_NOTCH = 100;

function stepZoom(steps: number): void {
  // Step from the APPLIED zoom (what's on screen). With no dynamic cap this
  // equals the stored preference, but reading it through getAppliedZoom keeps
  // the hotkeys honest if a ceiling ever comes back.
  const applied = getAppliedZoom();
  const next = clamp(+(applied + steps * ZOOM_STEP).toFixed(2), ZOOM_MIN, getEffectiveMaxZoom());
  // At either end of the band this is a no-op.
  if (next === applied) return;
  setAppearance({ ...getAppearance(), uiZoom: next });
}

function inTerminal(target: EventTarget | null): boolean {
  return target instanceof Element && !!target.closest(".terminal-pane, .xterm");
}

/** Install the global wheel + keyboard zoom handlers; returns a teardown. */
export function initZoomHotkeys(): () => void {
  let acc = 0;

  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey && !e.metaKey) return;
    // Always swallow the browser's page-zoom default. Over a terminal the
    // instance already handled (and stopped) the event for its font size;
    // reaching here over the pane chrome (tab strip / key row) is a no-op.
    e.preventDefault();
    if (inTerminal(e.target)) return;
    // deltaMode 1 = line scrolling; normalize roughly to pixels.
    acc += e.deltaY * (e.deltaMode === 1 ? 33 : 1);
    const steps = Math.trunc(acc / WHEEL_NOTCH);
    if (steps === 0) return;
    acc -= steps * WHEEL_NOTCH;
    stepZoom(-steps); // wheel up (negative delta) zooms in
  }

  function onKey(e: KeyboardEvent) {
    if ((!e.ctrlKey && !e.metaKey) || e.altKey) return;
    // Inside a terminal the keystroke belongs to the pty; use ctrl+wheel over
    // the terminal (font) or these keys anywhere else (zoom).
    if (inTerminal(e.target)) return;
    if (e.key === "=" || e.key === "+") {
      e.preventDefault();
      stepZoom(1);
    } else if (e.key === "-" || e.key === "_") {
      e.preventDefault();
      stepZoom(-1);
    } else if (e.key === "0") {
      e.preventDefault();
      const a = getAppearance();
      if (a.uiZoom !== 1) setAppearance({ ...a, uiZoom: 1 });
    }
  }

  window.addEventListener("wheel", onWheel, { passive: false });
  window.addEventListener("keydown", onKey);
  return () => {
    window.removeEventListener("wheel", onWheel);
    window.removeEventListener("keydown", onKey);
  };
}
