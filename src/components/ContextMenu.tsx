import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

export interface CtxItem {
  label: string;
  icon?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
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
}: {
  x: number;
  y: number;
  items: CtxItem[];
  /** Optional heading naming what the menu acts on. For triggers that carry no
   *  text of their own — a rail tile is an avatar, so "Rename workspace" alone
   *  never says WHICH one. */
  title?: string;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  // Clamp into the viewport once the menu has a size.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 8)),
      top: Math.max(4, Math.min(y, window.innerHeight - r.height - 8)),
    });
  }, [x, y]);

  useEffect(() => {
    // WebKitGTK paints a composited overflow scrollbar above fixed portal
    // content, regardless of the menu's z-index. Keep the sidebar scrollable,
    // but make its scrollbar transparent for the lifetime of the menu.
    document.documentElement.classList.add("ctx-menu-open");

    const down = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", down, true);
    window.addEventListener("contextmenu", down, true);
    window.addEventListener("keydown", key);
    window.addEventListener("blur", onClose);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("contextmenu", down, true);
      window.removeEventListener("keydown", key);
      window.removeEventListener("blur", onClose);
      document.documentElement.classList.remove("ctx-menu-open");
    };
  }, [onClose]);

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

  return createPortal(
    <div
      className="ctx-menu"
      style={pos}
      ref={ref}
      role="menu"
      aria-label={title}
      onKeyDown={onKeyDown}
    >
      {title && <div className="ctx-title">{title}</div>}
      {items.map((it) => (
        <button
          key={it.label}
          type="button"
          role="menuitem"
          tabIndex={-1}
          className={`ctx-item${it.danger ? " danger" : ""}`}
          disabled={it.disabled}
          onClick={() => {
            onClose();
            it.onSelect();
          }}
        >
          {it.icon}
          <span>{it.label}</span>
        </button>
      ))}
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
