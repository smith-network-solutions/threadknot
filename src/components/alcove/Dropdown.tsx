// Shared Alcove menu: its glass surface grows out of the trigger's actual box.
import { CSSProperties, ReactNode, RefObject, KeyboardEventHandler, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

export type DropdownPlacement =
  | "bottom-center"
  | "bottom-left"
  | "bottom-right"
  | "top-left"
  | "top-center"
  | "top-right"
  | "left-top"
  | "left-bottom"
  | "right-top"
  | "right-bottom";
export type DropdownTailPosition =
  | "top-left"
  | "top-right"
  | "bottom-left"
  | "bottom-right"
  | "left-top"
  | "left-bottom"
  | "right-top"
  | "right-bottom";

type DropdownProps = {
  trigger?: ReactNode;
  children: ReactNode;
  placement?: DropdownPlacement;
  tail?: boolean;
  tailPosition?: DropdownTailPosition;
  className?: string;
  menuClassName?: string;
  offset?: number;
  onOpenChange?: (open: boolean) => void;
  /** Merged into the portalled menu. Used to hand it sizing facts (how many
      rows it is about to hold) that CSS cannot work out for itself. */
  menuStyle?: CSSProperties;
  menuRole?: "menu" | "dialog";
  menuLabel?: string;
  closeOnSelect?: boolean;
  /** For menus owned elsewhere, such as the New chat workspace chooser. */
  anchor?: HTMLElement;
  open?: boolean;
  onCloseComplete?: () => void;
  menuRef?: RefObject<HTMLDivElement>;
  onKeyDown?: KeyboardEventHandler<HTMLDivElement>;
};

const defaultTail: Record<DropdownPlacement, DropdownTailPosition> = {
  "bottom-center": "top-left",
  "bottom-left": "top-left",
  "bottom-right": "top-right",
  "top-left": "bottom-left",
  "top-center": "bottom-left",
  "top-right": "bottom-right",
  "left-top": "right-top",
  "left-bottom": "right-bottom",
  "right-top": "left-top",
  "right-bottom": "left-bottom",
};

export function Dropdown({
  trigger,
  children,
  placement = "bottom-left",
  tail = false,
  tailPosition,
  className = "",
  menuClassName = "",
  offset = 0,
  onOpenChange,
  menuStyle,
  menuRole = "menu",
  menuLabel,
  closeOnSelect = true,
  anchor,
  open: controlledOpen,
  onCloseComplete,
  menuRef,
  onKeyDown,
}: DropdownProps) {
  const [internalOpen, setInternalOpen] = useState(false);
  const open = controlledOpen ?? internalOpen;
  const setOpen = (value: boolean) => {
    if (controlledOpen === undefined) setInternalOpen(value);
    else onOpenChange?.(value);
  };
  const [present, setPresent] = useState(open);
  const [position, setPosition] = useState({ top: 0, left: 0, tailCenter: 0 });
  const root = useRef<HTMLDivElement>(null);
  const triggerSlot = useRef<HTMLSpanElement>(null);
  const internalMenu = useRef<HTMLDivElement>(null);
  const menu = menuRef ?? internalMenu;
  const content = useRef<HTMLDivElement>(null);
  const animations = useRef<Animation[]>([]);
  const face = useRef<HTMLElement | null>(null);
  const triggerElement = () => anchor ?? root.current?.querySelector<HTMLElement>("button, [role='button']") ?? root.current;
  const restingTrigger = () => anchor?.parentElement ?? triggerSlot.current;

  const updatePosition = () => {
    // The slot has the button's resting layout, independent of its press scale.
    // Anchoring to the pressed button moves the destination when that scale settles.
    const triggerBounds = restingTrigger()?.getBoundingClientRect();
    const menuElement = menu.current;
    if (!triggerBounds || !menuElement) return;
    // The expanded surface occupies the button's footprint, keeping one edge
    // anchored there instead of leaving a vacant button below a detached popover.
    const horizontal = placement.startsWith("left-") || placement.startsWith("right-");
    const gap = offset - (horizontal ? triggerBounds.width : triggerBounds.height);
    const inset = 10;
    // Called only outside the morph. Preserve fractional CSS pixels at the handoff.
    const { width: menuWidth, height: menuHeight } = menuElement.getBoundingClientRect();
    const next = {
      "bottom-center": {
        top: triggerBounds.bottom + gap,
        left: triggerBounds.left + triggerBounds.width / 2 - menuWidth / 2,
      },
      "bottom-left": {
        top: triggerBounds.bottom + gap,
        left: triggerBounds.left - inset,
      },
      "bottom-right": {
        top: triggerBounds.bottom + gap,
        left: triggerBounds.right - menuWidth + inset,
      },
      "top-left": {
        top: triggerBounds.top - menuHeight - gap,
        left: triggerBounds.left - inset,
      },
      "top-center": {
        top: triggerBounds.top - menuHeight - gap,
        left: triggerBounds.left + triggerBounds.width / 2 - menuWidth / 2,
      },
      "top-right": {
        top: triggerBounds.top - menuHeight - gap,
        left: triggerBounds.right - menuWidth + inset,
      },
      "left-top": {
        top: triggerBounds.top - inset,
        left: triggerBounds.left - menuWidth - gap,
      },
      "left-bottom": {
        top: triggerBounds.bottom - menuHeight + inset,
        left: triggerBounds.left - menuWidth - gap,
      },
      "right-top": {
        top: triggerBounds.top - inset,
        left: triggerBounds.right + gap,
      },
      "right-bottom": {
        top: triggerBounds.bottom - menuHeight + inset,
        left: triggerBounds.right + gap,
      },
    }[placement];
    // Keep it on screen. A menu taller than the room above its trigger would
    // otherwise be placed at a negative top and get clipped by the window.
    const clamped = {
      top: Math.max(inset, Math.min(next.top, window.innerHeight - menuHeight - inset)),
      left: Math.max(inset, Math.min(next.left, window.innerWidth - menuWidth - inset)),
    };
    setPosition({
      ...clamped,
      tailCenter: triggerBounds.left + triggerBounds.width / 2 - clamped.left,
    });
    // Set these before the first paint, including the frame that starts a morph.
    menuElement.style.top = `${clamped.top}px`;
    menuElement.style.left = `${clamped.left}px`;
  };

  useLayoutEffect(() => {
    const panel = menu.current;
    const body = content.current;
    const button = triggerElement();
    if (!present || !panel || !body || !button) return;

    // Read the current frame before cancelling, so reversing mid-flight is continuous.
    const interrupted = animations.current.some((animation) =>
      animation.playState === "running" || animation.playState === "paused");
    const currentBox = panel.getBoundingClientRect();
    const currentStyle = getComputedStyle(panel);
    const currentRadius = currentStyle.borderRadius;
    const currentBackground = currentStyle.backgroundColor;
    const currentCornerShape = currentStyle.getPropertyValue("corner-shape") || "round";
    const currentOpacity = getComputedStyle(body).opacity;
    const currentBlur = getComputedStyle(body).filter;
    const faceOpacity = face.current ? getComputedStyle(face.current).opacity : "0";
    const faceBlur = face.current ? getComputedStyle(face.current).filter : "blur(10px)";
    animations.current.forEach((animation) => animation.cancel());
    face.current?.remove();
    face.current = null;
    body.style.width = "";
    body.style.height = "";
    panel.removeAttribute("data-morphing");
    updatePosition();

    const destination = panel.getBoundingClientRect();
    const restingSource = restingTrigger()?.getBoundingClientRect() ?? button.getBoundingClientRect();
    const source = open ? button.getBoundingClientRect() : restingSource;
    const buttonStyle = getComputedStyle(button);
    const panelStyle = getComputedStyle(panel);
    const radius = (value: string) => {
      const pixels = value.endsWith("%") ? parseFloat(value) / 100 * Math.min(source.width, source.height) : parseFloat(value);
      return `${Math.min(pixels || 0, source.width / 2, source.height / 2)}px`;
    };
    const buttonRadius = [buttonStyle.borderTopLeftRadius, buttonStyle.borderTopRightRadius,
      buttonStyle.borderBottomRightRadius, buttonStyle.borderBottomLeftRadius].map(radius).join(" ");
    const geometry = (box: DOMRect, borderRadius: string, backgroundColor: string) => ({
      left: `${box.left}px`, top: `${box.top}px`, width: `${box.width}px`, height: `${box.height}px`,
      borderRadius, backgroundColor,
    });
    const small = geometry(source, buttonRadius, buttonStyle.backgroundColor);
    const large = geometry(destination, panelStyle.borderRadius, panelStyle.backgroundColor);
    const from = interrupted
      ? geometry(currentBox, currentRadius, currentBackground)
      : open ? small : large;
    const buttonCornerShape = buttonStyle.getPropertyValue("corner-shape") || "round";
    const menuCornerShape = panelStyle.getPropertyValue("corner-shape") || "round";

    const finish = () => {
      if (!open) {
        // Keep the finished animations and the frozen contents until React removes
        // the portal. Cancelling here exposes the full-size menu for one frame.
        setPresent(false);
        onCloseComplete?.();
        return;
      }
      animations.current.forEach((animation) => animation.cancel());
      animations.current = [];
      face.current?.remove();
      face.current = null;
      body.style.width = "";
      body.style.height = "";
      panel.removeAttribute("data-morphing");
      updatePosition();
    };
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      finish();
      return;
    }

    // Freeze the contents at their natural size while only the glass box changes size.
    // Text never stretches or rewraps into a narrow column during the transition.
    body.style.width = `${body.getBoundingClientRect().width}px`;
    body.style.height = `${body.getBoundingClientRect().height}px`;
    panel.dataset.morphing = "true";

    // Carry the actual button face into the panel. Inline computed styles preserve
    // its appearance after leaving contextual selectors in the composer/sidebar.
    const copy = button.cloneNode(true) as HTMLElement;
    const originals = [button, ...button.querySelectorAll("*")];
    const copies = [copy, ...copy.querySelectorAll("*")];
    originals.forEach((original, index) => {
      const clone = copies[index] as HTMLElement | SVGElement;
      const style = getComputedStyle(original);
      for (const property of style) clone.style.setProperty(property, style.getPropertyValue(property));
      clone.removeAttribute("id");
      clone.style.setProperty("animation", "none");
      clone.style.setProperty("transition", "none");
    });
    copy.setAttribute("aria-hidden", "true");
    copy.inert = true;
    Object.assign(copy.style, {
      position: "absolute", left: "0", top: "0", right: "auto", bottom: "auto", margin: "0",
      width: `${restingSource.width}px`, height: `${restingSource.height}px`,
      opacity: "1", transform: "none", transformOrigin: "0 0", translate: "none", scale: "none",
      background: "transparent", boxShadow: "none", backdropFilter: "none", pointerEvents: "none",
    });
    panel.append(copy);
    face.current = copy;

    const duration = open ? 540 : 400;
    const to = open ? large : small;
    // Width leads height slightly, and the corners soften through the stretch.
    // Sample a damped spring so the surface settles without scaling its contents.
    const spring = (time: number, damping: number, frequency: number) =>
      time === 0 ? 0 : time === 1 ? 1 : 1 - Math.exp(-damping * time) *
        (Math.cos(frequency * time) + damping / frequency * Math.sin(frequency * time));
    const interpolate = (start: string, end: string, progress: number) =>
      parseFloat(start) + (parseFloat(end) - parseFloat(start)) * progress;
    const radii = (value: string) => {
      const parts = value.split(" ").map(parseFloat);
      return [parts[0], parts[1] ?? parts[0], parts[2] ?? parts[0], parts[3] ?? parts[1] ?? parts[0]];
    };
    const startRadii = radii(from.borderRadius);
    const endRadii = radii(to.borderRadius);
    const frames = Array.from({ length: 41 }, (_, index) => {
      const time = index / 40;
      const across = spring(time, 9.5, 7.5);
      const along = spring(time, 9, 6);
      const softness = Math.sin(Math.PI * time) ** 2 * (open ? 20 : 12);
      return {
        offset: time,
        left: `${interpolate(from.left, to.left, across)}px`,
        top: `${interpolate(from.top, to.top, along)}px`,
        width: `${interpolate(from.width, to.width, across)}px`,
        height: `${interpolate(from.height, to.height, along)}px`,
        borderRadius: startRadii.map((radius, i) =>
          `${Math.max(0, radius + (endRadii[i] - radius) * along + softness)}px`).join(" "),
      };
    });
    // Let the browser interpolate colors separately from the sampled geometry.
    const motion = panel.animate(frames, { duration, fill: "both" });
    const material = panel.animate([
      { backgroundColor: from.backgroundColor,
        cornerShape: interrupted ? currentCornerShape : open ? buttonCornerShape : menuCornerShape },
      { backgroundColor: to.backgroundColor, cornerShape: open ? menuCornerShape : buttonCornerShape },
    ], { duration, easing: "cubic-bezier(0.2, 0.7, 0.2, 1)", fill: "both" });
    const reveal = body.animate(open
      ? [{ opacity: interrupted ? currentOpacity : 0, filter: interrupted ? currentBlur : "blur(10px)" },
        { opacity: 1, filter: "blur(0px)", offset: 0.8 }, { opacity: 1, filter: "blur(0px)" }]
      : [{ opacity: currentOpacity, filter: currentBlur },
        { opacity: 0, filter: "blur(10px)", offset: 0.38 }, { opacity: 0, filter: "blur(10px)" }],
    { duration, fill: "both" });
    const label = copy.animate(open
      ? [{ opacity: interrupted ? faceOpacity : 1, filter: interrupted ? faceBlur : "blur(0px)" },
        { opacity: 0, filter: "blur(9px)", offset: 0.32 }, { opacity: 0, filter: "blur(9px)" }]
      : interrupted
        ? [{ opacity: faceOpacity, filter: faceBlur }, { opacity: 1, filter: "blur(0px)" }]
        : [{ opacity: 0, filter: "blur(9px)" }, { opacity: 0, filter: "blur(9px)", offset: 0.35 },
          { opacity: 1, filter: "blur(0px)", offset: 0.92 }, { opacity: 1, filter: "blur(0px)" }],
    { duration, fill: "both" });
    // Keep the icon at its button's location as the glass flows around it.
    const facePosition = copy.animate(frames.map((frame) => {
      const settle = spring(frame.offset, 9.5, 7.5);
      const x = source.left + (restingSource.left - source.left) * settle;
      const y = source.top + (restingSource.top - source.top) * settle;
      const scaleX = 1 + (source.width / restingSource.width - 1) * (1 - settle);
      const scaleY = 1 + (source.height / restingSource.height - 1) * (1 - settle);
      return {
        offset: frame.offset,
        transform: `translate(${x - parseFloat(frame.left)}px, ${y - parseFloat(frame.top)}px) scale(${scaleX}, ${scaleY})`,
      };
    }), { duration, fill: "both" });
    animations.current = [motion, material, reveal, label, facePosition];
    motion.onfinish = finish;
  }, [open, present]);

  useLayoutEffect(() => {
    if (!present) return;
    const previousOpacity = anchor?.style.opacity;
    if (anchor) anchor.style.opacity = "0";
    return () => {
      animations.current.forEach((animation) => animation.cancel());
      animations.current = [];
      face.current?.remove();
      face.current = null;
      if (anchor) anchor.style.opacity = previousOpacity ?? "";
    };
  }, [present, anchor]);

  useLayoutEffect(() => {
    if (open) setPresent(true);
  }, [open]);

  useLayoutEffect(() => {
    if (!present) return;
    const reposition = () => {
      if (menu.current?.hasAttribute("data-morphing")) return;
      updatePosition();
    };
    reposition();
    window.addEventListener("resize", reposition);
    window.addEventListener("scroll", reposition, true);
    // Reposition whenever the menu's own box changes, which is what the one-
    // shot measurement above could not account for.
    const observer = new ResizeObserver(reposition);
    if (content.current) observer.observe(content.current);
    const slot = restingTrigger();
    if (slot) observer.observe(slot);
    // And once more after the webfont lands: a face swap reflows the rows
    // without necessarily resizing the menu's border box.
    let cancelled = false;
    void document.fonts?.ready.then(() => {
      if (!cancelled) reposition();
    });
    return () => {
      cancelled = true;
      observer.disconnect();
      window.removeEventListener("resize", reposition);
      window.removeEventListener("scroll", reposition, true);
    };
  }, [present, placement, offset]);

  useLayoutEffect(() => {
    const button = triggerElement();
    button?.setAttribute("aria-expanded", String(open));
    button?.setAttribute("aria-haspopup", menuRole);
  }, [open, menuRole]);

  useEffect(() => {
    onOpenChange?.(open);
  }, [open, onOpenChange]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (
        !root.current?.contains(event.target as Node) &&
        !anchor?.contains(event.target as Node) &&
        !menu.current?.contains(event.target as Node)
      )
        setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  return (
    <div
      className={`dropdown ${className} ${open ? "is-open" : ""} ${present ? "is-present" : ""}`}
      ref={root}
      style={anchor ? { display: "contents" } : undefined}
    >
      {!anchor && <span
        ref={triggerSlot}
        className="dropdown-trigger"
        onClick={() => {
          if (open) setOpen(false);
          else {
            setPresent(true);
            setOpen(true);
          }
        }}
      >
        {trigger}
      </span>}
      {present &&
        createPortal(
          <div
            ref={menu}
            className={`dropdown-menu glass-surface corner-smoothing ${menuClassName}`}
            data-placement={placement}
            data-tail-position={tailPosition ?? defaultTail[placement]}
            data-state={open ? "open" : "closing"}
            role={menuRole}
            aria-label={menuLabel}
            onKeyDown={onKeyDown}
            onClick={() => { if (closeOnSelect) setOpen(false); }}
            style={{
              ...menuStyle,
              position: "fixed",
              top: position.top,
              left: position.left,
              right: "auto",
              bottom: "auto",
              "--dropdown-trigger-center": `${position.tailCenter}px`,
            } as CSSProperties}
          >
            {tail && <span className="dropdown-tail" aria-hidden="true" />}
            <div className="dropdown-content" ref={content}>{children}</div>
          </div>,
          document.body,
        )}
    </div>
  );
}
