import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Shared mechanics for the phone bottom sheet.
 *
 * Settings owned both of these privately until the agent panel needed the same
 * gesture and the same exit timing. They live here so the two sheets cannot
 * drift apart — a sheet that dismisses differently from the one beside it is
 * worse than either behaviour on its own.
 */

/** Downward pull, in px, past which releasing dismisses the sheet. */
const SHEET_DISMISS_PX = 96;
/** Downward speed, in px/ms, that dismisses regardless of distance. */
const SHEET_FLICK_SPEED = 0.5;

/**
 * Pull-down-to-dismiss.
 *
 * Deliberately bound to the grip and header only. If the whole sheet took the
 * gesture, every upward scroll inside a long list would be ambiguous at the top
 * of its scroll range, and the sheet would fight the content for it — the
 * classic bottom-sheet bug. The grip is the handle in both senses.
 *
 * Drag offset rides on the CSS `translate` property, which composes with — and
 * so never fights — the `transform` the open/close keyframes own.
 */
export function useSheetDrag(
  sheetRef: React.RefObject<HTMLDivElement | null>,
  onDismiss: () => void,
) {
  const start = useRef<{ y: number; t: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const setOffset = useCallback(
    (px: number) => {
      sheetRef.current?.style.setProperty("--sheet-drag", `${px}px`);
    },
    [sheetRef],
  );

  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    // The close and back buttons live in the drag zone; let them be buttons.
    if ((e.target as HTMLElement).closest("button")) return;
    // Without this the header text starts a native selection-drag a few pixels
    // in, which fires pointercancel and kills the gesture halfway down.
    e.preventDefault();
    start.current = { y: e.clientY, t: e.timeStamp };
    setDragging(true);
    // Capture can throw for a pointer the browser has already stopped
    // tracking; losing the capture is survivable, losing the handler is not.
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* keep dragging without capture */
    }
  }, []);

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!start.current) return;
      const dy = e.clientY - start.current.y;
      // Upward pulls get rubber-banded rather than lifting the sheet out of
      // its slot: there is nothing above it to reveal.
      setOffset(dy > 0 ? dy : dy / 4);
    },
    [setOffset],
  );

  const onPointerUp = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const from = start.current;
      if (!from) return;
      start.current = null;
      setDragging(false);
      const dy = e.clientY - from.y;
      const speed = dy / Math.max(1, e.timeStamp - from.t);
      if (dy > SHEET_DISMISS_PX || (speed > SHEET_FLICK_SPEED && dy > 24)) {
        // Leave the offset alone — the closing rule animates `translate` from
        // wherever the finger let go, so the sheet keeps the gesture's momentum.
        onDismiss();
      } else {
        setOffset(0);
      }
    },
    [onDismiss, setOffset],
  );

  // A cancel is the system taking the pointer away (a call arrives, the OS
  // claims the gesture). It carries no meaningful coordinates — reading
  // clientY off one gives 0, which reads as a big *upward* drag — so it can
  // never share the release path. Always put the sheet back.
  const onPointerCancel = useCallback(() => {
    if (!start.current) return;
    start.current = null;
    setDragging(false);
    setOffset(0);
  }, [setOffset]);

  return {
    dragging,
    dragHandlers: { onPointerDown, onPointerMove, onPointerUp, onPointerCancel },
  };
}

/**
 * Deferred unmount so a surface can play its exit animation.
 *
 * Unmount when the animation actually ends, not on a timer guessing how long it
 * took. A fixed timeout was a race: the `.closing` class only lands on the next
 * React render, so on a loaded machine the animation was still mid-flight when
 * the timer fired and the surface vanished in a pop. The timeout survives only
 * as a backstop for the case where no event ever arrives.
 */
export function useSheetClose(onClose: () => void) {
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const [closing, setClosing] = useState(false);
  const requestClose = useCallback(() => setClosing(true), []);

  useEffect(() => {
    if (!closing) return;
    const node = surfaceRef.current;
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      onClose();
    };
    // Both events bubble, so a transition on any descendant (a hover, a
    // spinner) would otherwise cut the exit short.
    const onEnd = (e: Event) => {
      if (e.target === node) finish();
    };
    node?.addEventListener("animationend", onEnd);
    node?.addEventListener("transitionend", onEnd);
    const timer = window.setTimeout(finish, 600);
    return () => {
      node?.removeEventListener("animationend", onEnd);
      node?.removeEventListener("transitionend", onEnd);
      window.clearTimeout(timer);
    };
  }, [closing, onClose]);

  return { surfaceRef, closing, requestClose };
}
