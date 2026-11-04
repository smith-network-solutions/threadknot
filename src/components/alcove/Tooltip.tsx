// The trigger half of the Alcove tooltip. Same props as the prototype's
// component, but instead of rendering a bubble of its own it hands its box and
// its label to the one shared bubble (see tooltipBus.ts / TooltipLayer.tsx),
// so moving between neighbouring controls slides that bubble across rather
// than crossfading two.
import { useEffect, useRef, type ReactNode } from "react";

import { hideTooltip, showTooltip, type TooltipPlacement } from "./tooltipBus";

export type { TooltipPlacement };

type TooltipProps = {
  children: ReactNode;
  content: ReactNode;
  className?: string;
  placement?: TooltipPlacement;
  tail?: boolean;
  disabled?: boolean;
};

export function Tooltip({
  children,
  content,
  className,
  placement = "bottom",
  tail = true,
  disabled = false,
}: TooltipProps) {
  const ref = useRef<HTMLSpanElement>(null);
  // Identity for the bus: a hide from a trigger the pointer has already left
  // must not cancel the tooltip that took its place.
  const owner = useRef({});
  // A click means the control did something — opened the attachment menu,
  // sent the message. Re-showing the label over that is noise, and with a
  // menu it reappears while the menu is still fading out. Stays suppressed
  // until the pointer actually leaves the control.
  const suppressed = useRef(false);

  // Nothing should outlive the trigger: unmounting mid-hover (the send button
  // becoming the stop button) would otherwise strand the bubble on screen.
  useEffect(() => () => hideTooltip(owner.current), []);

  function point() {
    const el = ref.current;
    if (!el || disabled || suppressed.current) return;
    const rect = el.getBoundingClientRect();
    showTooltip({
      owner: owner.current,
      rect: { top: rect.top, left: rect.left, width: rect.width, height: rect.height },
      content,
      placement,
      tail,
    });
  }

  return (
    <span
      ref={ref}
      className={["tooltip", className].filter(Boolean).join(" ")}
      data-placement={placement}
      onPointerEnter={point}
      // Re-aimed on move as well as entry: a control that grows under the
      // pointer (the send pill) would otherwise keep the bubble on its old box.
      onPointerMove={point}
      onPointerDown={() => {
        suppressed.current = true;
        hideTooltip(owner.current);
      }}
      onPointerLeave={() => {
        suppressed.current = false;
        hideTooltip(owner.current);
      }}
    >
      {children}
    </span>
  );
}
