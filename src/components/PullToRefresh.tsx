import { useEffect, useRef } from "react";
import { requestNativeReload } from "../lib/native";
import { RefreshIcon } from "./icons";

/** Pull distance (px, after damping) that commits a reload. */
const TRIGGER = 68;
/** Ceiling of the rubber band. */
const MAX = 104;
/** Vertical travel before the gesture is ours — taps and short drags stay free. */
const SLOP = 10;
/** Resting offset of the dial, just above the viewport. */
const REST = -44;

/**
 * Regions that own their own vertical drag, or where a reload would be an
 * accident rather than an intent. Terminals and the remote browser view feed
 * touches straight through to the far side; the composer is a text field.
 */
const EXCLUDE = [
  "[data-no-pull]",
  ".terminal-pane",
  ".browser-pane",
  ".composer",
  ".modal-backdrop",
  ".settings-pop",
  ".ctx-pop",
  '[role="dialog"]',
].join(",");

/**
 * Walk up from the touch target to the first real scroller. A pull only starts
 * when that scroller is already at its top (or nothing on the way up scrolls),
 * which is what keeps this out of the way of ordinary feed scrolling.
 */
function atScrollTop(target: Element | null): boolean {
  for (let el = target; el && el !== document.body; el = el.parentElement) {
    if (!(el instanceof HTMLElement)) continue;
    const oy = getComputedStyle(el).overflowY;
    if (oy !== "auto" && oy !== "scroll") continue;
    if (el.scrollHeight <= el.clientHeight + 1) continue;
    return el.scrollTop <= 0;
  }
  return true;
}

/** Linear up to the trigger, then increasingly stiff, capped at MAX. */
function damp(dy: number): number {
  const d = Math.max(0, dy - SLOP);
  if (d <= TRIGGER) return d;
  return Math.min(MAX, TRIGGER + (d - TRIGGER) * 0.4);
}

/**
 * Pull down from the top to reload the whole app — the gesture a phone browser
 * gives you for free, which this UI opts out of (`overscroll-behavior: none`,
 * a viewport-height root that never scrolls) and the native shell has no
 * chrome for. Inside the shell the reload is handed to the WebView so the
 * bundle is genuinely refetched; in a browser it is `location.reload()`.
 * Either way the app restores the thread it was on.
 */
export function PullToRefresh() {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    // Touch-primary devices only: a desktop mouse can't make this gesture, and
    // a stray trackpad drag reloading the app would be hostile.
    if (!window.matchMedia?.("(pointer: coarse)").matches) return;

    let tracking = false;
    let engaged = false;
    let refreshing = false;
    let startX = 0;
    let startY = 0;
    let dist = 0;

    const paint = (d: number) => {
      host.style.setProperty("--ptr-y", `${(REST + d).toFixed(1)}px`);
      host.style.setProperty("--ptr-o", Math.min(1, d / 36).toFixed(2));
      host.style.setProperty("--ptr-r", `${(d * 3).toFixed(0)}deg`);
      host.classList.toggle("ready", d >= TRIGGER);
    };

    const rest = () => {
      host.classList.add("settle");
      host.classList.remove("ready");
      host.style.setProperty("--ptr-y", `${REST}px`);
      host.style.setProperty("--ptr-o", "0");
      window.setTimeout(() => {
        if (!refreshing) host.classList.remove("on", "settle");
      }, 260);
    };

    const commit = () => {
      refreshing = true;
      host.classList.add("settle", "busy");
      host.classList.remove("ready");
      host.style.setProperty("--ptr-y", "8px");
      host.style.setProperty("--ptr-o", "1");
      if (!requestNativeReload()) {
        window.location.reload();
        return;
      }
      // The shell should have navigated by now; if its message went nowhere,
      // reload in place rather than leaving the dial spinning forever.
      window.setTimeout(() => window.location.reload(), 3000);
    };

    const onStart = (e: TouchEvent) => {
      if (refreshing || e.touches.length !== 1) return;
      const t = e.touches[0];
      const target = t.target instanceof Element ? t.target : null;
      if (!target || target.closest(EXCLUDE)) return;
      // Any open modal takes the screen; leave its content alone.
      if (document.querySelector(".modal-backdrop")) return;
      if (!atScrollTop(target)) return;
      tracking = true;
      engaged = false;
      dist = 0;
      startX = t.clientX;
      startY = t.clientY;
    };

    const onMove = (e: TouchEvent) => {
      if (!tracking || refreshing) return;
      if (e.touches.length !== 1) {
        tracking = false;
        rest();
        return;
      }
      const t = e.touches[0];
      const dy = t.clientY - startY;
      const dx = t.clientX - startX;
      if (!engaged) {
        // Upward or sideways first — this gesture belongs to the page.
        if (dy < -SLOP || Math.abs(dx) > Math.abs(dy)) {
          tracking = false;
          return;
        }
        if (dy < SLOP) return;
        engaged = true;
        host.classList.add("on");
        host.classList.remove("settle");
      }
      dist = damp(dy);
      if (e.cancelable) e.preventDefault();
      paint(dist);
    };

    const onEnd = () => {
      if (!tracking) return;
      tracking = false;
      if (!engaged) return;
      engaged = false;
      if (dist >= TRIGGER) commit();
      else rest();
    };

    /** The system took the gesture (notification shade, call, back swipe) —
     * that is not a request to reload the app. */
    const onCancel = () => {
      if (!tracking) return;
      tracking = false;
      if (!engaged) return;
      engaged = false;
      rest();
    };

    document.addEventListener("touchstart", onStart, { passive: true });
    document.addEventListener("touchmove", onMove, { passive: false });
    document.addEventListener("touchend", onEnd, { passive: true });
    document.addEventListener("touchcancel", onCancel, { passive: true });
    return () => {
      document.removeEventListener("touchstart", onStart);
      document.removeEventListener("touchmove", onMove);
      document.removeEventListener("touchend", onEnd);
      document.removeEventListener("touchcancel", onCancel);
    };
  }, []);

  return (
    <div className="ptr" ref={hostRef} aria-hidden="true">
      <div className="ptr-dial">
        <RefreshIcon size={17} />
      </div>
    </div>
  );
}
