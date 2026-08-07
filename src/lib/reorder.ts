// Drag-to-reorder for a vertical list, in pointer events so the same gesture
// works with a mouse in the desktop app and with a finger in the phone browser.
//
// Deliberately not HTML5 drag-and-drop: that is mouse-only on touch devices,
// and the sidebar already spends its `dragstart` on dragging a project OUT to
// its own window. Nothing here moves the DOM until the drop, so the rects
// measured mid-gesture stay honest and an item never slides out from under the
// pointer that is holding it.
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MouseEventHandler,
  type PointerEventHandler,
  type RefObject,
} from "react";

/** Mouse: movement that turns a press into a drag rather than a click. Small,
 *  because a mouse press that wanders 5px was never meant to be a click. */
const MOUSE_SLOP_PX = 5;
/** Touch: the press has to HOLD first, or every attempt to scroll a long rail
 *  would pick up whichever project the finger landed on. */
const TOUCH_HOLD_MS = 300;
/** Moving further than this before the hold fires means "scroll", not "drag". */
const TOUCH_SLOP_PX = 10;
/** Movement after the drag goes live that counts as actually dragging rather
 *  than a finger resting still. Below it the gesture is treated as a plain
 *  press, so a leisurely tap on a rail tile still switches project instead of
 *  being swallowed as a zero-distance drag. */
const JITTER_PX = 4;
/** How close to the scroller's edge the pointer has to get to drag the list
 *  along with it, and how fast it then travels. */
const EDGE_PX = 44;
const EDGE_SPEED_PX = 9;
/** Ceiling on the click guard, for gestures that end without a click at all. */
const CLICK_GUARD_MS = 400;

export interface ReorderHandleProps {
  onPointerDown?: PointerEventHandler<HTMLElement>;
  onClickCapture?: MouseEventHandler<HTMLElement>;
}

export interface ReorderDrag {
  /** Spread onto whatever the user grabs: the whole tile on the rail, the grip
   *  on a section header. */
  handleProps: (id: string) => ReorderHandleProps;
  /** The item being dragged, or null when no drag is live. */
  draggingId: string | null;
  /** The item the insertion marker sits against; null when the drop would not
   *  move anything (or nothing is being dragged). */
  dropId: string | null;
  /** Which edge of `dropId` the marker sits on. */
  dropSide: "before" | "after";
}

interface Session {
  pointerId: number;
  id: string;
  touch: boolean;
  startX: number;
  startY: number;
  /** Latest pointer y, also read by the edge-scroll frame. */
  y: number;
  /** Armed but not yet dragging until the slop/hold is cleared. */
  active: boolean;
  holdTimer: number | null;
  raf: number | null;
  /** DOM order snapshotted when the drag went live. The DOM does not move
   *  during a drag, so this stays valid until the drop. */
  ids: string[];
  from: number;
  /** The pointer travelled after the drag went live, so the gesture was a real
   *  drag and the click it ends on is not meant for the item. */
  moved: boolean;
}

interface ViewState {
  id: string;
  /** Insertion index into `ids` WITH the dragged item still in place. */
  to: number;
}

/**
 * Wire a list for drag-to-reorder. Items are found by their `data-reorder-id`
 * attribute inside `containerRef`, so the hook needs no registration step and
 * always reads the order that is actually on screen.
 *
 * `onCommit` receives the full new order of the ids it found; it is not called
 * when the drop lands the item back where it started.
 */
export function useReorderDrag(opts: {
  containerRef: RefObject<HTMLElement | null>;
  onCommit: (ids: string[]) => void;
  /** Off during a search or in a solo window, where the list on screen is not
   *  the list being ordered. */
  enabled?: boolean;
  /** The element to auto-scroll when the drag nears its edge. Defaults to the
   *  container, which is right for the rail (it scrolls itself) and wrong for
   *  section headers (the sidebar's scroller is an ancestor). */
  scrollRef?: RefObject<HTMLElement | null>;
}): ReorderDrag {
  const optsRef = useRef(opts);
  optsRef.current = opts;
  const session = useRef<Session | null>(null);
  const [view, setView] = useState<ViewState | null>(null);
  const suppressClick = useRef(false);
  const guardTimer = useRef<number | null>(null);

  const items = useCallback((): HTMLElement[] => {
    const root = optsRef.current.containerRef.current;
    if (!root) return [];
    return Array.from(root.querySelectorAll<HTMLElement>("[data-reorder-id]"));
  }, []);

  /** Which slot the pointer is over: the first item whose midpoint it has not
   *  passed, else the end of the list. */
  const slotAt = useCallback(
    (y: number): number => {
      const els = items();
      for (let i = 0; i < els.length; i += 1) {
        const r = els[i].getBoundingClientRect();
        if (y < r.top + r.height / 2) return i;
      }
      return els.length;
    },
    [items],
  );

  const end = useCallback(
    (commit: boolean) => {
      const s = session.current;
      session.current = null;
      if (!s) return;
      if (s.holdTimer !== null) window.clearTimeout(s.holdTimer);
      if (s.raf !== null) cancelAnimationFrame(s.raf);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("touchmove", blockScroll);
      document.body.classList.remove("reordering");
      if (s.active && s.moved) {
        // The gesture ends on the item it started on, so the browser fires a
        // click there: swallow it, or dropping a project onto a new slot would
        // also switch to it.
        suppressClick.current = true;
        if (guardTimer.current !== null) window.clearTimeout(guardTimer.current);
        guardTimer.current = window.setTimeout(() => {
          suppressClick.current = false;
          guardTimer.current = null;
        }, CLICK_GUARD_MS);
      }
      if (commit && s.active) {
        const to = slotAt(s.y);
        if (to !== s.from && to !== s.from + 1) {
          const next = [...s.ids];
          const [moved] = next.splice(s.from, 1);
          next.splice(to > s.from ? to - 1 : to, 0, moved);
          optsRef.current.onCommit(next);
        }
      }
      setView(null);
    },
    // Every listener below is defined before use at call time and never
    // changes identity within a session; the deps are the stable callbacks.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [slotAt],
  );

  const blockScroll = useCallback((e: TouchEvent) => {
    // React's own touchmove listener is passive, so the scroll has to be
    // refused here, on a non-passive window listener installed for the life of
    // the drag.
    if (e.cancelable) e.preventDefault();
  }, []);

  /** Drag the list along when the pointer sits against a scroller's edge. */
  const frame = useCallback(() => {
    const s = session.current;
    if (!s || !s.active) return;
    const scroller =
      optsRef.current.scrollRef?.current ?? optsRef.current.containerRef.current;
    if (scroller) {
      const r = scroller.getBoundingClientRect();
      const dy =
        s.y < r.top + EDGE_PX
          ? -EDGE_SPEED_PX
          : s.y > r.bottom - EDGE_PX
            ? EDGE_SPEED_PX
            : 0;
      if (dy !== 0) {
        const before = scroller.scrollTop;
        scroller.scrollTop += dy;
        if (scroller.scrollTop !== before) setView({ id: s.id, to: slotAt(s.y) });
      }
    }
    s.raf = requestAnimationFrame(frame);
  }, [slotAt]);

  const activate = useCallback(
    (s: Session) => {
      s.active = true;
      s.ids = items()
        .map((el) => el.dataset.reorderId ?? "")
        .filter(Boolean);
      s.from = s.ids.indexOf(s.id);
      if (s.from < 0) {
        end(false);
        return;
      }
      // A mouse only gets here by travelling past MOUSE_SLOP_PX, so the
      // gesture is already a drag; a touch gets here by holding still, and has
      // yet to prove it is one.
      if (!s.touch) s.moved = true;
      if (s.touch) navigator.vibrate?.(8);
      window.getSelection()?.removeAllRanges();
      // A cursor and a "nothing here is selectable" rule for the whole drag,
      // regardless of which element the pointer is over.
      document.body.classList.add("reordering");
      window.addEventListener("touchmove", blockScroll, { passive: false });
      s.raf = requestAnimationFrame(frame);
      setView({ id: s.id, to: slotAt(s.y) });
    },
    [items, end, blockScroll, frame, slotAt],
  );

  const onMove = useCallback(
    (e: PointerEvent) => {
      const s = session.current;
      if (!s || s.pointerId !== e.pointerId) return;
      s.y = e.clientY;
      if (!s.active) {
        const dx = Math.abs(e.clientX - s.startX);
        const dy = Math.abs(e.clientY - s.startY);
        if (s.touch) {
          // Wandering before the hold fires means the finger is scrolling.
          if (dx > TOUCH_SLOP_PX || dy > TOUCH_SLOP_PX) end(false);
        } else if (dx > MOUSE_SLOP_PX || dy > MOUSE_SLOP_PX) {
          activate(s);
        }
        return;
      }
      if (
        !s.moved &&
        (Math.abs(e.clientX - s.startX) > JITTER_PX ||
          Math.abs(e.clientY - s.startY) > JITTER_PX)
      ) {
        s.moved = true;
      }
      setView((prev) => {
        const to = slotAt(s.y);
        return prev && prev.to === to ? prev : { id: s.id, to };
      });
    },
    [end, activate, slotAt],
  );

  const onUp = useCallback(
    (e: PointerEvent) => {
      const s = session.current;
      if (!s || s.pointerId !== e.pointerId) return;
      end(true);
    },
    [end],
  );

  const onCancel = useCallback(() => end(false), [end]);
  const onKey = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape") end(false);
    },
    [end],
  );

  useEffect(() => {
    return () => {
      if (session.current) end(false);
      if (guardTimer.current !== null) window.clearTimeout(guardTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleProps = useCallback(
    (id: string): ReorderHandleProps => {
      if (optsRef.current.enabled === false) return {};
      return {
        onPointerDown: (event) => {
          if (event.pointerType === "mouse" && event.button !== 0) return;
          const target = event.target as Element | null;
          // Never start a reorder from a control that does something else:
          // the header's new-thread / pop-out / remove buttons, a rename box.
          if (
            target?.closest("input, textarea, select, [contenteditable='true']")
          ) {
            return;
          }
          if (session.current) end(false);
          // Two defaults to suppress on a mouse press: the text selection that
          // a drag across the sidebar would otherwise paint, and the HTML5
          // `dragstart` that the project header uses for popping a project out
          // into its own window. Touch keeps its default so the list can still
          // be scrolled until the hold fires.
          if (event.pointerType !== "touch") event.preventDefault();
          // The section header opens its context menu on a long press, and a
          // drag that starts on the grip is not that press.
          event.stopPropagation();
          const touch = event.pointerType === "touch";
          const s: Session = {
            pointerId: event.pointerId,
            id,
            touch,
            startX: event.clientX,
            startY: event.clientY,
            y: event.clientY,
            active: false,
            holdTimer: null,
            raf: null,
            ids: [],
            from: -1,
            moved: false,
          };
          session.current = s;
          window.addEventListener("pointermove", onMove);
          window.addEventListener("pointerup", onUp);
          window.addEventListener("pointercancel", onCancel);
          window.addEventListener("keydown", onKey);
          if (touch) {
            s.holdTimer = window.setTimeout(() => {
              s.holdTimer = null;
              if (session.current === s) activate(s);
            }, TOUCH_HOLD_MS);
          }
        },
        onClickCapture: (event) => {
          if (!suppressClick.current) return;
          suppressClick.current = false;
          if (guardTimer.current !== null) {
            window.clearTimeout(guardTimer.current);
            guardTimer.current = null;
          }
          event.preventDefault();
          event.stopPropagation();
        },
      };
    },
    [end, onMove, onUp, onCancel, onKey, activate],
  );

  // Where the insertion marker goes. A drop back into the item's own slot
  // moves nothing, so it draws no marker.
  let dropId: string | null = null;
  let dropSide: "before" | "after" = "before";
  const s = session.current;
  if (view && s && s.active) {
    if (view.to !== s.from && view.to !== s.from + 1) {
      if (view.to >= s.ids.length) {
        dropId = s.ids[s.ids.length - 1] ?? null;
        dropSide = "after";
      } else {
        dropId = s.ids[view.to] ?? null;
        dropSide = "before";
      }
    }
  }

  return { handleProps, draggingId: view?.id ?? null, dropId, dropSide };
}
