/**
 * Every hover tooltip in the app, drawn by us instead of the OS. The native one
 * is painted by the system from the `title` attribute, so no stylesheet can
 * reach it. Components keep writing plain `title=`; one document-level listener
 * lifts the attribute off whatever the pointer is over (which is what stops the
 * native tip) and puts it back on leave, so `title` stays the single source.
 *
 * "Name — detail" titles render as a two-line tip.
 */

const SHOW_DELAY_MS = 400;
/** Sliding along a row of icons shouldn't wait out the delay on each one. */
const WARM_DELAY_MS = 60;
const WARM_WINDOW_MS = 500;
const GAP = 8;
const EDGE = 8;
const SELECTOR = "[title]:not(iframe)";

export function installTooltips(): void {
  // No hover on touch: a tap would only leave a tip stranded on screen.
  if (!window.matchMedia("(hover: hover)").matches) return;

  const tip = document.createElement("div");
  tip.className = "tk-tip";
  tip.setAttribute("role", "tooltip");

  let held: Element | null = null;
  let text = "";
  let timer = 0;
  let watch = 0;
  let lastHidden = 0;

  // React rewrites `title` when the prop changes, even mid-hover ("1 running"
  // becoming "2 running"). Take the new value and keep the native tip suppressed.
  const observer = new MutationObserver(() => {
    const next = held?.getAttribute("title");
    if (!held || next == null) return;
    held.removeAttribute("title");
    observer.takeRecords();
    text = next;
    if (tip.isConnected) render();
  });

  function render() {
    const split = text.indexOf(" — ");
    tip.textContent = split < 0 ? text : text.slice(0, split);
    if (split >= 0) {
      const detail = document.createElement("span");
      detail.className = "tk-tip-detail";
      detail.textContent = text.slice(split + 3);
      tip.append(detail);
    }
    place();
  }

  function place() {
    if (!held) return;
    const anchor = held.getBoundingClientRect();
    const { width, height } = tip.getBoundingClientRect();
    const maxLeft = window.innerWidth - width - EDGE;
    const maxTop = window.innerHeight - height - EDGE;
    let left: number;
    let top: number;
    if (held.closest(".project-rail")) {
      // The rail hugs the window edge; its tips read outward, over the sidebar.
      left = anchor.right + GAP;
      top = anchor.top + (anchor.height - height) / 2;
    } else {
      left = anchor.left + (anchor.width - width) / 2;
      top = anchor.bottom + GAP;
      if (top > maxTop) top = anchor.top - height - GAP;
    }
    tip.style.left = `${Math.round(Math.max(EDGE, Math.min(left, maxLeft)))}px`;
    tip.style.top = `${Math.round(Math.max(EDGE, Math.min(top, maxTop)))}px`;
  }

  function show() {
    // The top layer hides anything outside a fullscreen element.
    (document.fullscreenElement ?? document.body).append(tip);
    render();
    requestAnimationFrame(() => tip.classList.add("on"));
  }

  /** Hide but keep holding: the pointer is still here, and giving `title` back
   *  now would let the native tip appear after a click. */
  function hide() {
    window.clearTimeout(timer);
    if (!tip.isConnected) return;
    tip.classList.remove("on");
    tip.remove();
    lastHidden = Date.now();
  }

  function release() {
    hide();
    window.clearInterval(watch);
    observer.disconnect();
    held?.setAttribute("title", text);
    held = null;
  }

  function hold(el: Element) {
    held = el;
    text = el.getAttribute("title") ?? "";
    el.removeAttribute("title");
    observer.observe(el, { attributes: true, attributeFilter: ["title"] });
    // A row can unmount under a still pointer, which fires no mouse event.
    watch = window.setInterval(() => {
      if (held && !held.isConnected) release();
    }, 250);
    const warm = Date.now() - lastHidden < WARM_WINDOW_MS;
    timer = window.setTimeout(show, warm ? WARM_DELAY_MS : SHOW_DELAY_MS);
  }

  document.addEventListener("mouseover", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const el = target.closest(SELECTOR);
    // `held` has no title right now, so closest() skips it: inside it, a match
    // only counts if it is a titled child rather than one of its ancestors.
    if (held?.contains(target) && !(el && held.contains(el))) return;
    if (held) release();
    if (el?.getAttribute("title")) hold(el);
  });
  document.documentElement.addEventListener("mouseleave", () => held && release());
  window.addEventListener("blur", () => held && release());
  // pointerdown, not mousedown: draggable rows preventDefault it, which cancels
  // the compatibility mouse events that would follow.
  for (const type of ["pointerdown", "keydown", "wheel"] as const) {
    document.addEventListener(type, hide, { capture: true, passive: true });
  }
}
