// One tooltip, many triggers.
//
// Every Alcove <Tooltip> used to carry its own bubble, so moving along a row
// of controls faded one out and another in — two things happening where the
// eye sees one. Instead each trigger publishes "point at me, with this label"
// here, and the single <TooltipLayer/> mounted at the root slides and resizes
// its one bubble to the new target, blur-swapping the words on the way.
//
// Deliberately a module-level value rather than context: the triggers are
// scattered across unrelated trees, and a hover must not re-render any of
// them — only the layer listens.
import type { ReactNode } from "react";

export type TooltipPlacement = "top" | "right" | "bottom" | "left";

export interface TooltipRequest {
  /** The trigger, so a stale hide from a trigger the pointer already left
   *  cannot cancel the tooltip that replaced it. */
  owner: object;
  /** Viewport coordinates of the trigger, read at hover time. */
  rect: { top: number; left: number; width: number; height: number };
  content: ReactNode;
  placement: TooltipPlacement;
  tail: boolean;
}

type Listener = (request: TooltipRequest | null) => void;

let current: TooltipRequest | null = null;
const listeners = new Set<Listener>();

/** How long the bubble keeps the stage after its trigger lets go, waiting to
 *  see whether the pointer is simply on its way to a neighbour.
 *
 *  Without it the hand-off is decided by event order, which is decided by
 *  whether the two controls happen to touch. Controls sharing an edge fire
 *  `pointerenter` on the next one before `pointerleave` on the last, so the
 *  bubble is still up and the layer slides it across. Controls with a gap
 *  between them — the rail tiles, anything across the header — put the leave
 *  first, which cleared the bubble, so the next hover came up as a fresh
 *  pop-in instead. Same gesture, two different motions, depending on spacing.
 *
 *  Short enough to read as "the tooltip is still up", long enough to cover
 *  crossing a gap at any speed a pointer realistically moves. */
const HANDOVER_MS = 90;
let pendingHide: ReturnType<typeof setTimeout> | undefined;

function cancelPendingHide() {
  if (pendingHide === undefined) return;
  clearTimeout(pendingHide);
  pendingHide = undefined;
}

function publish() {
  for (const listener of listeners) listener(current);
}

/** True when a request says exactly what the live one already says. Triggers
 *  re-aim on every pointer move, and republishing an identical request would
 *  restart the layer's label swap on each one. */
function same(a: TooltipRequest | null, b: TooltipRequest): boolean {
  return (
    a != null &&
    a.owner === b.owner &&
    a.content === b.content &&
    a.placement === b.placement &&
    a.tail === b.tail &&
    a.rect.top === b.rect.top &&
    a.rect.left === b.rect.left &&
    a.rect.width === b.rect.width &&
    a.rect.height === b.rect.height
  );
}

export function showTooltip(request: TooltipRequest): void {
  // Claims the bubble back from a hand-off in flight, which is what turns the
  // next hover into a slide rather than a pop-in. Before the `same` check:
  // re-entering the control the pointer just left publishes nothing new, but
  // it still has to call off that control's own pending hide.
  cancelPendingHide();
  if (same(current, request)) return;
  current = request;
  publish();
}

/** Release the bubble, unless another trigger has already taken it over.
 *  It does not come down immediately — see HANDOVER_MS. */
export function hideTooltip(owner: object): void {
  if (current?.owner !== owner) return;
  cancelPendingHide();
  pendingHide = setTimeout(() => {
    pendingHide = undefined;
    current = null;
    publish();
  }, HANDOVER_MS);
}

export function subscribeTooltip(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function currentTooltip(): TooltipRequest | null {
  return current;
}
