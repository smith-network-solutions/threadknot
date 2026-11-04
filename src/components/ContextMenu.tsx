import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

import { useDismissAnimation } from "../lib/useDismissAnimation";
import { useAlcove } from "../lib/useAlcove";
import { Dropdown } from "./alcove/Dropdown";

export interface CtxItem {
  label: string;
  icon?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
  /** Non-interactive label used to group related menu items. */
  kind?: "item" | "heading";
  /** Visually nests an item beneath the heading above it. */
  inset?: boolean;
  /** Starts a new command group without introducing another heading. */
  dividerBefore?: boolean;
  /** Trailing pill, for a one-word qualifier the label should not carry
   *  (e.g. "Local" on the rows that point at this very machine). */
  badge?: string;
  /** Colour family for the badge. "local" is the bright blue that means
   *  "this computer"; omit for the quiet default. */
  badgeTone?: "local";
  onSelect: () => void;
}

/**
 * In-app context menu (right-click). Rendered through a portal so it escapes
 * any transformed/overflow ancestor; one code path for the Tauri shell on
 * every platform AND plain browsers, so no native-menu capabilities needed.
 */
export function ContextMenu({
  x,
  y,
  items,
  title,
  onClose,
  className,
  anchor,
}: {
  x: number;
  y: number;
  items: CtxItem[];
  /** Optional heading naming what the menu acts on. For triggers that carry no
   *  text of their own — a rail tile is an avatar, so "Rename workspace" alone
   *  never says WHICH one. */
  title?: string;
  onClose: () => void;
  /** Extra class on the menu root, for a caller that wants its own size or
   *  shape. Used by "New chat in", which matches the composer's dropdowns. */
  className?: string;
  anchor?: HTMLElement;
}) {
  const alcove = useAlcove();
  const morph = alcove && !!anchor;
  const [morphOpen, setMorphOpen] = useState(true);
  const afterClose = useRef<(() => void) | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  // Every way out of this menu goes through `dismiss`, not `onClose`: the
  // parents all render it as `{point && <ContextMenu/>}`, so calling onClose
  // directly unmounts it on the spot and the exit animation never runs. The
  // menu now plays its own exit and tells the parent when it is over.
  const { closing, dismiss: dismissLegacy, onAnimationEnd } = useDismissAnimation(onClose);
  const dismiss = morph ? () => setMorphOpen(false) : dismissLegacy;

  // Clamp into the viewport once the menu has a size.
  useLayoutEffect(() => {
    if (morph) return;
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 8)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 8)),
    });
  }, [x, y, morph]);

  useEffect(() => {
    // WebKitGTK paints a composited overflow scrollbar above fixed portal
    // content, regardless of the menu's z-index. Keep the sidebar scrollable,
    // but make its scrollbar transparent for the lifetime of the menu.
    document.documentElement.classList.add("ctx-menu-open");

    const down = (e: MouseEvent) => {
      if (morph && anchor?.contains(e.target as Node)) return;
      if (ref.current && !ref.current.contains(e.target as Node)) dismiss();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") dismiss();
    };
    window.addEventListener("mousedown", down, true);
    window.addEventListener("contextmenu", down, true);
    window.addEventListener("keydown", key);
    window.addEventListener("blur", dismiss);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("contextmenu", down, true);
      window.removeEventListener("keydown", key);
      window.removeEventListener("blur", dismiss);
      document.documentElement.classList.remove("ctx-menu-open");
    };
  }, [dismiss]);

  // Keyboard access: move focus into the menu on open (so arrow keys and
  // Enter/Space work for keyboard users who activated the trigger), and hand
  // focus back to whatever was focused before the menu opened when it closes
  // by ANY path (select, outside click, Escape, blur) unless an onSelect has
  // intentionally moved focus elsewhere (e.g. opening a modal or renamer).
  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    firstEnabledItem(ref.current)?.focus();
    return () => {
      if (!opener || !opener.isConnected) return;
      const active = document.activeElement;
      const menuEl = ref.current;
      // Only pull focus back if nothing else has claimed it: focus is still
      // on a menu item, or it fell to the body when the menu was removed.
      if (!active || active === document.body || (menuEl && menuEl.contains(active))) {
        opener.focus();
      }
    };
  }, []);

  // Roving focus for arrow / Home / End. Enter and Space are handled natively
  // by the focused <button>. Escape is handled by the window listener above.
  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const enabled = enabledItems(ref.current);
    if (enabled.length === 0) return;
    const idx = enabled.indexOf(document.activeElement as HTMLButtonElement);
    let next = -1;
    switch (e.key) {
      case "ArrowDown":
        next = idx < 0 ? 0 : (idx + 1) % enabled.length;
        break;
      case "ArrowUp":
        next = idx <= 0 ? enabled.length - 1 : idx - 1;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = enabled.length - 1;
        break;
      default:
        return;
    }
    e.preventDefault();
    enabled[next]?.focus();
  };

  const contents = (
    <>
      {title && <div className="ctx-title">{title}</div>}
      {items.map((it, index) =>
        it.kind === "heading" ? (
          <div
            key={`heading:${it.label}:${index}`}
            className={`ctx-section${it.dividerBefore ? " divided" : ""}`}
          >
            {it.icon}
            <span>{it.label}</span>
          </div>
        ) : (
          <button
            key={`item:${it.label}:${index}`}
            type="button"
            role="menuitem"
            tabIndex={-1}
            className={`ctx-item${it.danger ? " danger" : ""}${it.inset ? " inset" : ""}${it.dividerBefore ? " divided" : ""}`}
            disabled={it.disabled}
            onClick={() => {
              if (morph) afterClose.current = it.onSelect;
              dismiss();
              if (!morph) it.onSelect();
            }}
          >
            {it.icon}
            <span className="ctx-label">{it.label}</span>
            {it.badge && (
              <span className={`ctx-badge${it.badgeTone ? ` ${it.badgeTone}` : ""}`}>
                {it.badge}
              </span>
            )}
          </button>
        ),
      )}
    </>
  );
  if (morph) return (
    <Dropdown
      anchor={anchor}
      open={morphOpen}
      onOpenChange={setMorphOpen}
      onCloseComplete={() => { onClose(); afterClose.current?.(); }}
      menuRef={ref}
      menuClassName={`ctx-menu ${className ?? ""}`}
      menuLabel={title}
      placement="bottom-left"
      onKeyDown={onKeyDown}
    >
      {contents}
    </Dropdown>
  );
  return createPortal(
    <div
      className={`ctx-menu${className ? ` ${className}` : ""}`}
      style={pos}
      ref={ref}
      role="menu"
      aria-label={title}
      data-state={closing ? "closing" : "open"}
      onAnimationEnd={onAnimationEnd}
      onKeyDown={onKeyDown}
    >
      {contents}
    </div>,
    document.body,
  );
}

/** Enabled menu-item buttons in DOM order. */
function enabledItems(container: HTMLElement | null): HTMLButtonElement[] {
  if (!container) return [];
  return Array.from(
    container.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled])'),
  );
}

function firstEnabledItem(container: HTMLElement | null): HTMLButtonElement | undefined {
  return enabledItems(container)[0];
}
