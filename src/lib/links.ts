import { openUrl } from "@tauri-apps/plugin-opener";

/**
 * Every link the app renders — markdown links from an agent, PR links, image
 * previews — is a plain `<a target="_blank">`. That works in a browser, but the
 * Tauri webview has no new-window handler, so WebKit silently drops the click
 * and the link looks dead. Route those clicks to the system browser instead.
 *
 * One capture-phase listener covers every anchor in the app, including ones
 * react-markdown creates at runtime.
 */

/** Schemes we hand to the OS. Anything else (data:, blob:, javascript:) is left alone. */
const OPENABLE = /^(https?|mailto|file):/i;

function isTauriEnv(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
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
    void openUrl(href).catch(() => {
      /* no system handler for this scheme — nothing better to do than ignore */
    });
  };

  document.addEventListener("click", onClick, true);
  return () => document.removeEventListener("click", onClick, true);
}
