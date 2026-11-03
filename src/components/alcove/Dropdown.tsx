// Ported verbatim from the Alcove design prototype (C:/Projects/alcove):
// the portalled glass popover. Shared by the Alcove skin; see src/styles/alcove/.
import { CSSProperties, ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
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
  trigger: ReactNode;
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
  tail = true,
  tailPosition,
  className = "",
  menuClassName = "",
  offset = 0,
  onOpenChange,
  menuStyle,
}: DropdownProps) {
  const [open, setOpen] = useState(false);
  const [present, setPresent] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0, tailCenter: 0 });
  const root = useRef<HTMLDivElement>(null);
  const menu = useRef<HTMLDivElement>(null);

  const updatePosition = () => {
    const triggerBounds = root.current?.getBoundingClientRect();
    const menuElement = menu.current;
    if (!triggerBounds || !menuElement) return;
    const gap = 12 + offset;
    const inset = 10;
    // offset dimensions intentionally ignore the menu's open-scale transform.
    // That keeps centered menus centered on their trigger while they animate.
    const menuWidth = menuElement.offsetWidth;
    const menuHeight = menuElement.offsetHeight;
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
  };

  useLayoutEffect(() => {
    if (!open) return;
    updatePosition();
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    // Reposition whenever the menu's own box changes, which is what the one-
    // shot measurement above could not account for.
    const observer = new ResizeObserver(updatePosition);
    if (menu.current) observer.observe(menu.current);
    // And once more after the webfont lands: a face swap reflows the rows
    // without necessarily resizing the menu's border box.
    let cancelled = false;
    void document.fonts?.ready.then(() => {
      if (!cancelled) updatePosition();
    });
    return () => {
      cancelled = true;
      observer.disconnect();
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [open, placement, offset]);

  useEffect(() => {
    onOpenChange?.(open);
  }, [open, onOpenChange]);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (
        !root.current?.contains(event.target as Node) &&
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
      className={`dropdown ${className} ${open ? "is-open" : ""}`}
      ref={root}
    >
      <span
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
      </span>
      {present &&
        createPortal(
          <div
            ref={menu}
            className={`dropdown-menu glass-surface corner-smoothing ${menuClassName}`}
            data-placement={placement}
            data-tail-position={tailPosition ?? defaultTail[placement]}
            data-state={open ? "open" : "closing"}
            role="menu"
            onClick={() => setOpen(false)}
            onAnimationEnd={(event) => {
              if (event.target === event.currentTarget && !open)
                setPresent(false);
            }}
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
            {children}
          </div>,
          document.body,
        )}
    </div>
  );
}
