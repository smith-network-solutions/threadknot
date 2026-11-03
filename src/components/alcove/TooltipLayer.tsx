// The one bubble every Alcove tooltip trigger shares. Mounted once, at the
// root; fed by tooltipBus.
//
// Three motions, and which one runs depends only on where the bubble already
// was:
//   hidden → shown   pops in on the target (scale + fade), no travel
//   shown  → shown   slides and resizes to the new target, blur-swapping the
//                    label halfway so the words change mid-flight
//   shown  → hidden  fades back down
//
// Position is left/top in viewport coordinates with the placement's own
// percentage offset left in CSS, so one inline pair of numbers drives all four
// directions. Width is written too, measured off a hidden copy of the incoming
// label: a bubble that is `width: auto` cannot animate between two labels.
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

import {
  currentTooltip,
  subscribeTooltip,
  type TooltipPlacement,
  type TooltipRequest,
} from "./tooltipBus";

/** The gap between trigger and bubble, matching the CSS this replaced. */
const GAP = 10;
/** Half the cross-fade: the label blurs out over this, then the new one in.
 *  Keep in step with .tooltip-layer__label's transition in alcove/glass.css. */
const SWAP_MS = 120;
/** Side padding, in JS because the width written below has to include it and
 *  a measured copy cannot carry a padding that depends on its own width. A
 *  long label gets more room at the ends, or the words crowd the pill's
 *  curve; a short one would only look slack with it. */
const PAD = 21;
const PAD_LONG = 29;
/** Where "long" starts: text wider than this takes the roomier padding. */
const LONG_TEXT = 150;

interface Box {
  left: number;
  top: number;
  width: number;
  pad: number;
  placement: TooltipPlacement;
  tail: boolean;
}

function place(request: TooltipRequest): { left: number; top: number } {
  const { rect, placement } = request;
  const cx = rect.left + rect.width / 2;
  const cy = rect.top + rect.height / 2;
  if (placement === "bottom") return { left: cx, top: rect.top + rect.height + GAP };
  if (placement === "top") return { left: cx, top: rect.top - GAP };
  if (placement === "right") return { left: rect.left + rect.width + GAP, top: cy };
  return { left: rect.left - GAP, top: cy };
}

export function TooltipLayer() {
  const [request, setRequest] = useState<TooltipRequest | null>(currentTooltip);
  const [box, setBox] = useState<Box | null>(null);
  const [label, setLabel] = useState<ReactNode>(null);
  const [visible, setVisible] = useState(false);
  const [swapping, setSwapping] = useState(false);
  const measureRef = useRef<HTMLSpanElement>(null);
  const bubbleRef = useRef<HTMLSpanElement>(null);
  const wasShowing = useRef(false);

  useEffect(() => subscribeTooltip(setRequest), []);

  useLayoutEffect(() => {
    if (!request) {
      setVisible(false);
      wasShowing.current = false;
      return;
    }
    // The hidden copy has already rendered with the incoming label, so this
    // is the text the bubble is about to hold. It carries no padding of its
    // own, so the pill's width is the text plus whichever padding its length
    // earns.
    const text = measureRef.current?.getBoundingClientRect().width ?? 0;
    const pad = text > LONG_TEXT ? PAD_LONG : PAD;
    const next: Box = {
      ...place(request),
      width: text + pad * 2,
      pad,
      placement: request.placement,
      tail: request.tail,
    };
    if (!wasShowing.current) {
      // Nothing on screen to travel from: place it, label it, and let it pop
      // in on the next frame. Flipping visible in the same commit would make
      // the browser animate left/top from wherever the last tooltip sat.
      setBox(next);
      setLabel(request.content);
      wasShowing.current = true;
      const frame = requestAnimationFrame(() => {
        // Read the laid-out box before the class lands. Without this the
        // browser never recalculates between "move it" and "transition it",
        // so the two coalesce and the bubble slides in from the last
        // tooltip's position instead of popping in on this one. A rAF alone
        // does not do it: the callback runs before that frame's style pass.
        void bubbleRef.current?.getBoundingClientRect();
        setVisible(true);
      });
      return () => cancelAnimationFrame(frame);
    }
    // Already up: move and resize now, change the words at the midpoint.
    setBox(next);
    setVisible(true);
    setSwapping(true);
    const timer = setTimeout(() => {
      setLabel(request.content);
      setSwapping(false);
    }, SWAP_MS);
    return () => clearTimeout(timer);
  }, [request]);

  if (!box && !request) return null;
  return (
    <span
      ref={bubbleRef}
      className={`tooltip-layer${visible ? " is-visible" : ""}${swapping ? " is-swapping" : ""}`}
      data-placement={box?.placement ?? "bottom"}
      style={
        box
          ? { left: box.left, top: box.top, width: box.width, paddingInline: box.pad }
          : undefined
      }
      role="tooltip"
      aria-hidden={!visible}
    >
      {box?.tail && <span className="tooltip-tail" aria-hidden="true" />}
      <span className="tooltip-layer__label">{label}</span>
      {/* Off-layout copy of whatever is arriving next, purely to be measured. */}
      <span className="tooltip-layer__measure" ref={measureRef} aria-hidden="true">
        {request?.content ?? label}
      </span>
    </span>
  );
}
