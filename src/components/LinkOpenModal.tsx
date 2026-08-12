import { useEffect, useState } from "react";
import { findThread, useStore } from "../state/store";
import { isQuickHomeProjectId } from "../lib/protocol";
import { LINK_CHOICE_EVENT, openExternally } from "../lib/links";
import { stageBrowserUrl } from "../lib/browserIntent";
import { GlobeIcon, XIcon } from "./icons";

/**
 * "Where should this link open?" — raised by the external-link handler for
 * every http(s) click (see links.ts). Two answers:
 *
 *  - Threadknot Browser: opens the active workspace's browser pane on the
 *    URL. Only offered when a real project workspace is in play; quick
 *    threads have no workspace panel to host the pane.
 *  - Default Browser: hands the URL to the OS.
 *
 * A styled in-app dialog, never a native prompt. Esc / backdrop / X cancel.
 */
export function LinkOpenModal() {
  const { state, dispatch } = useStore();
  const [href, setHref] = useState<string | null>(null);

  useEffect(() => {
    const onLink = (e: Event) => {
      const detail = (e as CustomEvent<{ href?: string }>).detail;
      if (detail?.href) setHref(detail.href);
    };
    window.addEventListener(LINK_CHOICE_EVENT, onLink);
    return () => window.removeEventListener(LINK_CHOICE_EVENT, onLink);
  }, []);

  useEffect(() => {
    if (href === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setHref(null);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [href]);

  if (href === null) return null;

  // The pane lives in a project workspace; resolve one from the open thread.
  const threadProjectId = state.activeThreadId
    ? findThread(state, state.activeThreadId)?.projectId
    : undefined;
  const paneProjectId =
    threadProjectId &&
    !isQuickHomeProjectId(threadProjectId) &&
    state.projects.some((p) => p.id === threadProjectId)
      ? threadProjectId
      : null;

  const openInPane = () => {
    if (!paneProjectId) return;
    stageBrowserUrl(paneProjectId, href);
    dispatch({ type: "workspace", projectId: paneProjectId, tab: "browser" });
    setHref(null);
  };

  const openInSystem = () => {
    openExternally(href);
    setHref(null);
  };

  return (
    <div className="modal-backdrop" onClick={() => setHref(null)}>
      <div className="modal link-open-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>Open link</span>
          <button className="icon-btn" aria-label="Close" onClick={() => setHref(null)}>
            <XIcon size={14} />
          </button>
        </div>
        <div className="link-open-url" title={href}>
          {href}
        </div>
        <div className="link-open-actions">
          <button
            type="button"
            className="settings-toggle"
            disabled={!paneProjectId}
            title={
              paneProjectId
                ? "Open in the workspace's browser pane"
                : "Open a workspace thread to use the Threadknot browser"
            }
            onClick={openInPane}
          >
            <GlobeIcon size={15} />
            Threadknot Browser
          </button>
          <button type="button" className="settings-toggle" onClick={openInSystem}>
            Default Browser
          </button>
        </div>
      </div>
    </div>
  );
}
