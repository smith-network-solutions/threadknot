import {
  useEffect,
  useRef,
  type MouseEventHandler,
  type PointerEventHandler,
} from "react";

export interface MenuPoint {
  x: number;
  y: number;
}

interface LongPressHandlers {
  onPointerDown: PointerEventHandler<HTMLElement>;
  onPointerMove: PointerEventHandler<HTMLElement>;
  onPointerUp: PointerEventHandler<HTMLElement>;
  onPointerCancel: PointerEventHandler<HTMLElement>;
  onClickCapture: MouseEventHandler<HTMLElement>;
}

/** The handler bag plus a way to disarm the click guard early. Spread the
 *  bag onto the element; call `disarm` when the thing the press opened goes
 *  away, so the guard can never outlive the gesture and eat a later tap. */
export interface LongPressMenu extends LongPressHandlers {
  disarm: () => void;
}

const HOLD_MS = 520;
const MOVE_SLOP_PX = 12;
const CLICK_GUARD_MS = 800;
/** How far a click has to land from the hold that armed the guard before it
 *  counts as a deliberate second tap rather than the hold's own ghost click.
 *  Wider than MOVE_SLOP_PX: the ghost lands within a couple of pixels, while
 *  a real follow-up tap on a neighbouring control is tens of pixels away. */
const GHOST_CLICK_SLOP_PX = 24;

interface PendingHold {
  pointerId: number;
  x: number;
  y: number;
  fired: boolean;
  timer: number;
}

/**
 * Turn a stationary touch hold into the same menu-opening point used by a
 * mouse contextmenu event. Native scrolling still wins: moving more than a
 * few pixels (or a browser pointercancel) abandons the hold.
 */
export function useLongPressMenu(
  onOpen: (point: MenuPoint) => void,
  enabled = true,
): LongPressMenu {
  const callbackRef = useRef(onOpen);
  const pendingRef = useRef<PendingHold | null>(null);
  const suppressClickRef = useRef(false);
  const clickGuardRef = useRef<number | null>(null);
  /** Where the hold that armed the guard happened, so the guard can tell a
   *  ghost click apart from a real second tap. */
  const suppressAtRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  callbackRef.current = onOpen;

  function clearPending() {
    const pending = pendingRef.current;
    if (pending) window.clearTimeout(pending.timer);
    pendingRef.current = null;
  }

  function clearClickGuard() {
    if (clickGuardRef.current != null) {
      window.clearTimeout(clickGuardRef.current);
      clickGuardRef.current = null;
    }
    suppressClickRef.current = false;
  }

  useEffect(
    () => () => {
      clearPending();
      clearClickGuard();
    },
    [],
  );

  const onPointerDown: PointerEventHandler<HTMLElement> = (event) => {
    if (!enabled || event.pointerType !== "touch" || event.button !== 0) return;
    const target = event.target as Element;
    if (target.closest("input, textarea, select, [contenteditable='true']")) return;

    // A new touch means the previous gesture is over, so the ghost click it
    // might have left is never coming. Disarming here is what makes the guard
    // EXACT rather than a race against CLICK_GUARD_MS: a ghost click is always
    // dispatched before the next pointerdown, so it still gets swallowed,
    // while a deliberate follow-up tap — the settle tick on the row you just
    // long-pressed — always lands. Without this the guard could eat the first
    // tap after every long-press.
    clearClickGuard();
    clearPending();
    const pending: PendingHold = {
      pointerId: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      fired: false,
      timer: 0,
    };
    pending.timer = window.setTimeout(() => {
      if (pendingRef.current !== pending) return;
      pending.fired = true;
      suppressClickRef.current = true;
      suppressAtRef.current = { x: pending.x, y: pending.y };
      if (clickGuardRef.current != null) window.clearTimeout(clickGuardRef.current);
      clickGuardRef.current = window.setTimeout(clearClickGuard, CLICK_GUARD_MS);
      navigator.vibrate?.(8);
      window.getSelection()?.removeAllRanges();
      callbackRef.current({ x: pending.x, y: pending.y });
    }, HOLD_MS);
    pendingRef.current = pending;
  };

  const onPointerMove: PointerEventHandler<HTMLElement> = (event) => {
    const pending = pendingRef.current;
    if (!pending || pending.pointerId !== event.pointerId || pending.fired) return;
    if (
      Math.abs(event.clientX - pending.x) > MOVE_SLOP_PX ||
      Math.abs(event.clientY - pending.y) > MOVE_SLOP_PX
    ) {
      clearPending();
    }
  };

  const finish: PointerEventHandler<HTMLElement> = (event) => {
    const pending = pendingRef.current;
    if (!pending || pending.pointerId !== event.pointerId) return;
    if (pending.fired && event.cancelable) event.preventDefault();
    clearPending();
  };

  const onClickCapture: MouseEventHandler<HTMLElement> = (event) => {
    if (!suppressClickRef.current) return;
    // The guard exists to swallow the ghost click a long-press leaves on the
    // element that OWNS the press — the row, the project header. On touch,
    // `finish` already preventDefaults the pointerup, so that ghost click
    // usually never arrives and the guard is still armed when the user makes
    // their next deliberate tap. If that tap lands on a nested control (the
    // settle tick, "new thread", the kebab), it is intentional: let it through
    // and disarm, or the first tap after every long-press silently does
    // nothing.
    //
    // "Nested" alone isn't enough to call it deliberate: `.project-head` holds
    // the press ON `.project-toggle`, so its ghost click is nested too, and
    // letting that through would collapse the section the press just opened a
    // menu for. A ghost lands where the finger was; a real second tap lands
    // somewhere else. Require both.
    const target = event.target as Element | null;
    const nested = target?.closest("button, a, input, [role='menuitem']");
    const { x, y } = suppressAtRef.current;
    const movedAway =
      Math.abs(event.clientX - x) > GHOST_CLICK_SLOP_PX ||
      Math.abs(event.clientY - y) > GHOST_CLICK_SLOP_PX;
    if (nested && nested !== event.currentTarget && movedAway) {
      clearClickGuard();
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    clearClickGuard();
  };

  return {
    onPointerDown,
    onPointerMove,
    onPointerUp: finish,
    onPointerCancel: finish,
    onClickCapture,
    disarm: clearClickGuard,
  };
}
