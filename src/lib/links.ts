import { openPath, openUrl } from "@tauri-apps/plugin-opener";

/**
 * Every link the app renders — markdown links from an agent, PR links, image
 * previews — is a plain `<a target="_blank">`. That works in a browser, but the
 * Tauri webview has no new-window handler, so WebKit silently drops the click
 * and the link looks dead.
 *
 * http(s) links raise LINK_CHOICE_EVENT so LinkOpenModal can ask where to open
 * (Threadknot browser pane vs the system default browser). Other openable
 * schemes (mailto, tel, file) go straight to the OS: the pane can't render
 * them anyway.
 *
 * One capture-phase listener covers every anchor in the app, including ones
 * react-markdown creates at runtime.
 */

/** Fired with detail `{ href: string }` when an http(s) link is clicked. */
export const LINK_CHOICE_EVENT = "threadknot:link-choice";

/** Schemes we can open. Anything else (data:, blob:, javascript:) is left alone. */
const OPENABLE = /^(https?|mailto|tel|file):/i;

function isTauriEnv(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Open in the OS: default browser / mail client / file handler.
 *  Failures are logged, not swallowed — a dead link with no console trace is
 *  how the missing opener scope went unnoticed for weeks. */
export function openExternally(href: string): void {
  if (/^file:/i.test(href)) {
    // openUrl's scope only covers web-ish schemes; files go through openPath.
    let path = href;
    try {
      path = decodeURIComponent(new URL(href).pathname);
    } catch {
      /* keep the raw href; openPath will report it */
    }
    void openPath(path).catch((e: unknown) => console.warn("openPath failed:", path, e));
  } else {
    void openUrl(href).catch((e: unknown) => console.warn("openUrl failed:", href, e));
  }
}

export function installExternalLinkHandler(): () => void {
  // Browsers already do the right thing with target="_blank".
  if (!isTauriEnv()) return () => {};

  const onClick = (event: MouseEvent) => {
    if (event.defaultPrevented || event.button !== 0) return;
    // Modifier clicks are browser gestures; let the webview handle (or ignore) them.
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;

    const target = event.target;
    if (!(target instanceof Element)) return;
    const anchor = target.closest("a[href]");
    if (!(anchor instanceof HTMLAnchorElement)) return;
    // Hidden `<a download>` elements are how the Files/Artifacts panes save a
    // file — hijacking those would break downloading.
    if (anchor.hasAttribute("download")) return;

    const href = anchor.getAttribute("href") ?? "";
    // Relative and in-page hrefs address the app itself, not the outside world.
    if (!OPENABLE.test(href)) return;

    event.preventDefault();
    if (/^https?:/i.test(href)) {
      window.dispatchEvent(new CustomEvent(LINK_CHOICE_EVENT, { detail: { href } }));
    } else {
      openExternally(href);
    }
  };

  document.addEventListener("click", onClick, true);
  return () => document.removeEventListener("click", onClick, true);
}
