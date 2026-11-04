import { useCallback, useEffect, useRef, useState } from "react";
import type { AnimationEvent } from "react";

/**
 * Let a popover animate out before it leaves the tree.
 *
 * A popover whose parent renders it as `{open && <Thing/>}` has no exit: the
 * element is gone from the DOM on the same tick as the click, so there is
 * nothing left for an exit animation to run on. This holds the close for the
 * length of that animation — the caller marks the element `data-state=
 * "closing"`, the CSS plays, and only then does `onDone` fire and the parent
 * unmount it.
 *
 * Driven by `animationend` rather than a timer so the stylesheet owns the
 * duration: each skin can spend as long as it likes, and the reduced-motion
 * rule that collapses every Alcove animation to 1ms is respected for free.
 *
 * The timer is the fallback, not the mechanism. `animationend` never arrives
 * if the element has no exit animation in the current skin, or if the window
 * blurred and the frame loop stopped — and a popover that never unmounts is a
 * worse bug than one that does not animate.
 */
export function useDismissAnimation(
  onDone: () => void,
  fallbackMs = 400,
): {
  /** True once dismissal has started. Drives `data-state="closing"`. */
  closing: boolean;
  /** Start the exit. Safe to call repeatedly; only the first one counts. */
  dismiss: () => void;
  /** Put on the animating element. */
  onAnimationEnd: (event: AnimationEvent<Element>) => void;
} {
  const [closing, setClosing] = useState(false);
  const timer = useRef<number | null>(null);
  // Read at fire time, so a caller that passes a fresh closure every render
  // does not restart the exit or fire a stale one.
  const latest = useRef(onDone);
  latest.current = onDone;

  const finish = useCallback(() => {
    if (timer.current !== null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    // Back to idle BEFORE handing control back. A popover often outlives its
    // own dismissal: the usage chip keeps its component mounted and only the
    // portal comes and goes, so a `closing` left set meant the next open
    // rendered straight into the exit animation — the popover was visible
    // once, then never again.
    setClosing(false);
    latest.current();
  }, []);

  const dismiss = useCallback(() => {
    if (timer.current !== null) return;
    setClosing(true);
    timer.current = window.setTimeout(finish, fallbackMs);
  }, [finish, fallbackMs]);

  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  const onAnimationEnd = useCallback(
    (event: AnimationEvent<Element>) => {
      // Only the element's own animation; a row inside it finishing its hover
      // transition must not tear the menu down early.
      if (event.target === event.currentTarget && timer.current !== null) finish();
    },
    [finish],
  );

  return { closing, dismiss, onAnimationEnd };
}
