import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { nameInitials } from "./MachineAvatar";

/** What the floating preview shows for an avatar-ish anchor. */
export interface AvatarPreviewLook {
  image?: string;
  color?: string;
  name: string;
}

/** Mouse handlers to spread onto the anchor element (empty when the look has
 *  nothing worth previewing: no image and no accent color). */
export interface AvatarHoverProps {
  onMouseEnter?: (e: React.MouseEvent<HTMLElement>) => void;
  onMouseLeave?: () => void;
}

/**
 * Does this device have a real hover? A touch tap fires `mouseenter` with no
 * matching `mouseleave`, so a preview opened by a tap sticks over the UI
 * until you happen to tap elsewhere. That was survivable while previews hung
 * off decorative badges; on the project rail the avatar IS the button, so
 * every project switch flung a 176px badge across the chat list.
 *
 * Resolved once and cached: hover capability does not meaningfully change
 * mid-session, and this is called for every avatar on screen.
 */
let hoverCapable: boolean | null = null;
function deviceCanHover(): boolean {
  if (hoverCapable === null) {
    hoverCapable =
      typeof window === "undefined" || typeof window.matchMedia !== "function"
        ? true
        : window.matchMedia("(hover: hover)").matches;
  }
  return hoverCapable;
}

const SHOW_DELAY_MS = 160;
const MIN_DIAMETER = 96;
const MAX_DIAMETER = 176;
const VIEW_MARGIN = 8;
const ANCHOR_GAP = 10;
/** Vertical room the name pill needs where it overlaps the bottom edge. */
const LABEL_OVERHANG = 12;

/**
 * One shared mechanism behind every profile-circle hover preview: after a
 * short intent delay, a portaled circular badge (about 4x the anchor size,
 * clamped) pops beside the anchor with the image (or color + initials), a
 * ring in the entity's accent color, and its name in a pill overlapping the
 * bottom edge. The portal is fixed-position on document.body. Every anchor
 * that uses it (sidebar rows, settings cards, the thread header chip) renders
 * at 1x, and even one inside the zoomed message feed would be safe: Chromium
 * reports getBoundingClientRect in scaled viewport px, which maps 1:1 onto
 * fixed coordinates. Either way no zoom math is needed.
 */
export function useAvatarHoverPreview(look: AvatarPreviewLook): {
  hoverProps: AvatarHoverProps;
  portal: ReactNode;
} {
  const { image, color, name } = look;
  // Initials-only avatars with no image and no color: skip entirely. So is
  // every avatar on a touch-only device, where the preview can be opened but
  // never dismissed.
  const enabled = (!!image || !!color) && deviceCanHover();
  const timer = useRef<number | null>(null);
  const anchorRef = useRef<HTMLElement | null>(null);
  const [box, setBox] = useState<{ top: number; left: number; d: number } | null>(null);

  const hide = useCallback(() => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    setBox(null);
  }, []);

  // Delay before showing so drive-by cursor passes never flicker a preview.
  const onMouseEnter = useCallback((e: React.MouseEvent<HTMLElement>) => {
    anchorRef.current = e.currentTarget;
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      timer.current = null;
      const el = anchorRef.current;
      if (!el || !el.isConnected) return;
      const r = el.getBoundingClientRect();
      const d = Math.round(
        Math.min(MAX_DIAMETER, Math.max(MIN_DIAMETER, r.width * 4)),
      );
      // Prefer sitting to the anchor's right; flip left near the right edge,
      // then clamp fully inside the viewport.
      let left = r.right + ANCHOR_GAP;
      if (left + d + VIEW_MARGIN > window.innerWidth) left = r.left - ANCHOR_GAP - d;
      left = Math.max(VIEW_MARGIN, Math.min(left, window.innerWidth - d - VIEW_MARGIN));
      const top = Math.max(
        VIEW_MARGIN,
        Math.min(
          r.top + r.height / 2 - d / 2,
          window.innerHeight - d - LABEL_OVERHANG - VIEW_MARGIN,
        ),
      );
      setBox({ top, left, d });
    }, SHOW_DELAY_MS);
  }, []);

  // Any scroll while visible means the anchor moved out from under us: hide.
  useEffect(() => {
    if (!box) return;
    const onScroll = () => hide();
    window.addEventListener("scroll", onScroll, true);
    return () => window.removeEventListener("scroll", onScroll, true);
  }, [box, hide]);

  // Never leave a timer running past unmount (the portal itself unmounts
  // with the anchor's component, since this hook lives inside it).
  useEffect(
    () => () => {
      if (timer.current !== null) clearTimeout(timer.current);
    },
    [],
  );

  let portal: ReactNode = null;
  if (box && enabled) {
    const style: CSSProperties = {
      top: box.top,
      left: box.left,
      width: box.d,
      height: box.d,
    };
    if (color) (style as Record<string, string | number>)["--preview-ring"] = color;
    portal = createPortal(
      <div className="avatar-preview" style={style} aria-hidden>
        <span
          className="avatar-preview-circle"
          style={{ fontSize: Math.round(box.d * 0.32) }}
        >
          {image ? <img src={image} alt="" /> : <span>{nameInitials(name)}</span>}
        </span>
        <span className="avatar-preview-name">{name}</span>
      </div>,
      document.body,
    );
  }

  return {
    hoverProps: enabled ? { onMouseEnter, onMouseLeave: hide } : {},
    portal,
  };
}
