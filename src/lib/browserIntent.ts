/** Hand a URL to a project's Threadknot browser pane.
 *
 * The link chooser (LinkOpenModal) may open the pane and stage the URL before
 * the pane has mounted or its socket is live, so this is a mailbox rather
 * than a plain event: the pane takes the staged URL on mount / on the event /
 * when its connection goes live, whichever happens last. */

export const BROWSER_INTENT_EVENT = "threadknot:browser-intent";

let pendingUrl: string | null = null;
let pendingProjectId: string | null = null;

/** Stage `url` for `projectId`'s browser pane and poke any mounted pane. */
export function stageBrowserUrl(projectId: string, url: string): void {
  pendingProjectId = projectId;
  pendingUrl = url;
  window.dispatchEvent(new CustomEvent(BROWSER_INTENT_EVENT));
}

/** Claim the staged URL if it belongs to `projectId`; clears the mailbox. */
export function takeBrowserUrl(projectId: string): string | null {
  if (pendingUrl === null || pendingProjectId !== projectId) return null;
  const url = pendingUrl;
  pendingUrl = null;
  pendingProjectId = null;
  return url;
}
