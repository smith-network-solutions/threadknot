// Per-pane zoom hotkeys, wired once from App: ctrl/cmd + wheel zooms the pane
// under the cursor; ctrl/cmd + = / - / 0 step the last pane the user clicked
// in. Each pane keeps its own zoom through the appearance store (persisted +
// broadcast, same as the settings stepper). Panes declare themselves with a
// data-zoom-pane attribute; anything unmatched (thread header, composer,
// toasts) falls through to "feed", which is exactly the old behavior.
// Terminals opt out: ctrl+wheel over a terminal scales that terminal's own
// font (see TerminalInstance); keystrokes inside xterm stay with the pty.
// Mac trackpad pinch arrives as ctrl+wheel, so the same path covers it, and
// the whole mechanism is plain CSS/JS in the one webview, so Mac/Linux/
// Windows behave identically.
// Touch-only devices (phone layout) keep the historical feed-only behavior:
// pane vars stay at 1 and nothing about the mobile experience changes.

import {
  clamp,
  getAppliedZoom,
  getEffectiveMaxZoom,
  getPaneZoom,
  PANE_KINDS,
  setPaneZoom,
  ZOOM_MIN,
  ZOOM_STEP,
  type PaneKind,
} from "./appearance";

/** deltaY per zoom step: one detented wheel notch in Chromium. Trackpad
 *  pinches stream small fractional deltas, so accumulate up to the notch. */
const WHEEL_NOTCH = 100;

function stepZoom(kind: PaneKind, steps: number): void {
  // Step from the APPLIED zoom (what's on screen). Only the feed ever had a
  // dynamic-cap story; reading through getAppliedZoom keeps that path honest
  // if a ceiling ever comes back.
  const applied = kind === "feed" ? getAppliedZoom() : getPaneZoom(kind);
  const next = clamp(+(applied + steps * ZOOM_STEP).toFixed(2), ZOOM_MIN, getEffectiveMaxZoom());
  // At either end of the band this is a no-op.
  if (next === applied) return;
  setPaneZoom(kind, next);
}

function inTerminal(target: EventTarget | null): boolean {
  return target instanceof Element && !!target.closest(".terminal-pane, .xterm");
}

/** The About screen's full-window takeover carries its own zoom, the same way
 *  a terminal carries its own font size. It is portaled to <body>, so it is
 *  inside no pane at all and would otherwise fall through to "feed" and zoom
 *  the conversation behind it. Flagged on the root element rather than sniffed
 *  from the event target, because the target is whatever happens to hold focus
 *  in there and may well be <body>. */
function cabinetUp(): boolean {
  return document.documentElement.dataset.legacyCircuit === "on";
}

/** Resolve which zoomable pane an event landed in. */
function paneAt(target: EventTarget | null): PaneKind {
  if (!(target instanceof Element)) return "feed";
  const kind = target.closest("[data-zoom-pane]")?.getAttribute("data-zoom-pane");
  return kind && (PANE_KINDS as readonly string[]).includes(kind) ? (kind as PaneKind) : "feed";
}

/** Install the global wheel + keyboard zoom handlers; returns a teardown. */
export function initZoomHotkeys(): () => void {
  // Phone/tablet layout: keep the pre-pane behavior byte for byte. Same idiom
  // as the terminal's coarse-pointer check.
  const touchOnly = window.matchMedia("(hover: none) and (pointer: coarse)").matches;
  // Keyboard zoom follows the last pane the user clicked in.
  let activePane: PaneKind = "feed";
  let acc = 0;
  let accPane: PaneKind = "feed";

  const resolve = (t: EventTarget | null): PaneKind => (touchOnly ? "feed" : paneAt(t));

  function onPointerDown(e: PointerEvent) {
    // Capture phase, so panes that stopPropagation internally still register.
    // Clicking a terminal does not steal the keyboard-zoom target: its own
    // ctrl+wheel font sizing covers it and ctrl+-/= belong to the pty there.
    if (!touchOnly && !inTerminal(e.target)) activePane = paneAt(e.target);
  }

  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey && !e.metaKey) return;
    // Always swallow the browser's page-zoom default. Over a terminal the
    // instance already handled (and stopped) the event for its font size;
    // reaching here over the pane chrome (tab strip / key row) is a no-op.
    e.preventDefault();
    if (inTerminal(e.target) || cabinetUp()) return;
    const pane = resolve(e.target);
    // Leftover delta from one pane must not zoom the next one the cursor
    // crosses into; restart the accumulator at pane boundaries.
    if (pane !== accPane) {
      acc = 0;
      accPane = pane;
    }
    // deltaMode 1 = line scrolling; normalize roughly to pixels.
    acc += e.deltaY * (e.deltaMode === 1 ? 33 : 1);
    const steps = Math.trunc(acc / WHEEL_NOTCH);
    if (steps === 0) return;
    acc -= steps * WHEEL_NOTCH;
    stepZoom(pane, -steps); // wheel up (negative delta) zooms in
  }

  function onKey(e: KeyboardEvent) {
    if ((!e.ctrlKey && !e.metaKey) || e.altKey) return;
    // Inside a terminal the keystroke belongs to the pty; use ctrl+wheel over
    // the terminal (font) or these keys anywhere else (zoom).
    if (inTerminal(e.target) || cabinetUp()) return;
    const pane = touchOnly ? "feed" : activePane;
    if (e.key === "=" || e.key === "+") {
      e.preventDefault();
      stepZoom(pane, 1);
    } else if (e.key === "-" || e.key === "_") {
      e.preventDefault();
      stepZoom(pane, -1);
    } else if (e.key === "0") {
      e.preventDefault();
      if (getPaneZoom(pane) !== 1) setPaneZoom(pane, 1);
    }
  }

  window.addEventListener("pointerdown", onPointerDown, true);
  window.addEventListener("wheel", onWheel, { passive: false });
  window.addEventListener("keydown", onKey);
  return () => {
    window.removeEventListener("pointerdown", onPointerDown, true);
    window.removeEventListener("wheel", onWheel);
    window.removeEventListener("keydown", onKey);
  };
}
