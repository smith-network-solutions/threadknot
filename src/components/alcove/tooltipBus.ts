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
  if (same(current, request)) return;
  current = request;
  publish();
}

/** Hide, unless another trigger has already taken the bubble over. */
export function hideTooltip(owner: object): void {
  if (current?.owner !== owner) return;
  current = null;
  publish();
}

export function subscribeTooltip(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function currentTooltip(): TooltipRequest | null {
  return current;
}
