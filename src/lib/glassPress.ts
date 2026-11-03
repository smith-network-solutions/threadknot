// Where the press came from.
//
// The Alcove skin's press turns a surface's exposure up radially, out of the
// point the pointer actually landed on — so the gesture has to tell CSS where
// that was. One passive pointerdown listener writes the hit point onto the
// pressed element as percentages; `glass-press.css` reads them as the centre
// of the ripple's radial-gradient and animates its radius from there.
//
// Everything else about the press is CSS. This exists only because a
// stylesheet cannot know where a click happened.

/** Must stay in step with the selector list in styles/alcove/glass-press.css. */
const PRESSABLE = [
  ".thread-head .thread-title",
  ".thread-head .title-edit",
  ".sidebar-action",
  ".thread-row",
  ".empty-project-trigger",
  ".empty-model-trigger",
  ".quick-mode-tabs button",
  ".chat-find button",
  ".cset-trigger",
  ".pill-trigger",
  ".ai-trigger",
  ".mic-btn",
  ".send-btn",
  ".ctx-item",
  ".pill-opt",
  ".agent-option",
  ".slash-option",
  ".composer-anchor .composer-card",
  ".composer-anchor .composer-icon",
  ".composer-anchor .model-select",
  ".composer-anchor .send-control",
  ".composer-anchor .composer-image__remove",
  ".attachment-menu__option",
  ".model-menu__item",
  ".model-menu__permission-option",
  ".glass",
  ".glass-surface",
].join(",");

function onPointerDown(event: PointerEvent) {
  // Only while the skin is on; every other theme pays nothing but this check.
  if (document.documentElement.dataset.theme !== "alcove") return;
  const target = event.target;
  if (!(target instanceof Element)) return;
  const pressed = target.closest(PRESSABLE);
  if (!(pressed instanceof HTMLElement)) return;
  const rect = pressed.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return;
  pressed.style.setProperty("--press-x", `${((event.clientX - rect.left) / rect.width) * 100}%`);
  pressed.style.setProperty("--press-y", `${((event.clientY - rect.top) / rect.height) * 100}%`);
}

let installed = false;

/** Called once at boot. Safe to call again; it only ever adds one listener. */
export function installGlassPress() {
  if (installed || typeof document === "undefined") return;
  installed = true;
  // Capture, so a handler that stops propagation (menus do) cannot cost the
  // press its origin. Passive: this never calls preventDefault.
  document.addEventListener("pointerdown", onPointerDown, { capture: true, passive: true });
}
