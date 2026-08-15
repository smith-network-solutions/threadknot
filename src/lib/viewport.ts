import { useEffect, useState } from "react";

/**
 * Phone-width cutoff. The whole app agrees on this number: below it the sidebar
 * goes off-canvas, the composer switches to its compact form, and Settings
 * becomes a bottom sheet. Kept in sync by hand with the `max-width: 767px`
 * media queries in styles.css — a JS branch and a CSS branch that disagree
 * produce a layout no one designed.
 */
export const MOBILE_MAX_WIDTH = 767;

const MOBILE_QUERY = `(max-width: ${MOBILE_MAX_WIDTH}px)`;

/**
 * Below this, the gap between the two viewports is browser furniture, not a
 * keyboard. It has to sit ABOVE the tallest piece of furniture, and the tallest
 * is iOS's input accessory bar at ~45px — which the old value of 40 let
 * through, so a focused field with no keyboard up still lifted the composer by
 * the height of the bar. The smallest real phone keyboard (an SE in portrait)
 * is ~216px, so anything in this range is furniture with room to spare.
 */
const KEYBOARD_MIN_INSET = 120;

/**
 * Publish how much of the layout viewport the on-screen keyboard covers as
 * `--kb-inset` on <html>. The composer's bottom padding and the feed's bottom
 * reserve both read it (see the phone block in styles.css), so the input rides
 * above the keyboard and the newest message stays clear of the input.
 *
 * Two platforms, one number:
 *  - Chrome/Android honours `interactive-widget=resizes-content` (index.html):
 *    the layout viewport shrinks with the visual one, so both terms below drop
 *    together, this reports 0, and the ordinary layout is already correct.
 *  - iOS ignores that directive and shrinks only the VISUAL viewport, leaving a
 *    bottom-anchored composer behind the keyboard. There the difference IS the
 *    keyboard, and the CSS lifts the composer by exactly that much.
 *
 * offsetTop is subtracted because Safari may also pan the visual viewport to
 * reveal the focused field: anything it pushes off the top is not room we have.
 *
 * Returns a teardown. Safe to call where visualViewport is missing (older
 * WebKit) — it simply leaves the inset at its 0px default.
 */
export function installKeyboardInset(): () => void {
  const vv = typeof window === "undefined" ? undefined : window.visualViewport;
  if (!vv) return () => {};

  let frame: number | null = null;
  let published = -1;

  function measure() {
    frame = null;
    // offsetTop is clamped at 0 before subtracting: Safari panning the visual
    // viewport down (positive) genuinely costs us room, but a NEGATIVE offset —
    // which WebKit reports transiently while the keyboard animates and while
    // the page rubber-bands — would be subtracted into extra inset and lift the
    // composer above the keyboard by that much.
    const overlap = window.innerHeight - vv!.height - Math.max(0, vv!.offsetTop);
    const inset = overlap > KEYBOARD_MIN_INSET ? Math.round(overlap) : 0;
    // The keyboard animates open, so this fires many times per gesture; writing
    // only on a change keeps it from restyling the feed on every frame.
    if (inset === published) return;
    published = inset;
    document.documentElement.style.setProperty("--kb-inset", `${inset}px`);
  }

  function schedule() {
    if (frame === null) frame = window.requestAnimationFrame(measure);
  }

  vv.addEventListener("resize", schedule);
  vv.addEventListener("scroll", schedule);
  measure();

  return () => {
    vv.removeEventListener("resize", schedule);
    vv.removeEventListener("scroll", schedule);
    if (frame !== null) window.cancelAnimationFrame(frame);
  };
}

/** True on phone-width viewports, tracking resizes and device rotation. */
export function useIsMobile(): boolean {
  const [mobile, setMobile] = useState(
    () => typeof window !== "undefined" && window.matchMedia(MOBILE_QUERY).matches,
  );
  useEffect(() => {
    const mq = window.matchMedia(MOBILE_QUERY);
    const on = () => setMobile(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return mobile;
}
