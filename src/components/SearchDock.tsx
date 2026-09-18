import { memo, useEffect, useRef } from "react";
import { findThread, useStore } from "../state/store";
import {
  HERMES_HOME_PROJECT_ID,
  isQuickHomeProjectId,
  SMART_SEARCH_MODELS,
  type Thread,
} from "../lib/protocol";
import { timeAgo } from "../lib/format";
import { AgentMark, ArrowDownIcon, ArrowUpIcon, SearchIcon, XIcon } from "./icons";
import "../styles/search-dock.css";

/** AI search results parked beside the chat (`state.searchDock`). Opened by
 *  picking a result in the search modal; every click here swaps the chat in
 *  the main pane while the list stays put, so you can step through the
 *  candidates until you recognise the right one. The X closes the dock and
 *  leaves whichever thread is open. */
export const SearchDock = memo(function SearchDock() {
  const { state, dispatch, actions } = useStore();
  const dock = state.searchDock;
  const listRef = useRef<HTMLDivElement>(null);

  // Follow the open chat: if the user switches threads from the sidebar while
  // the dock is up, the highlight moves (or clears) rather than lying.
  useEffect(() => {
    if (!dock) return;
    const open = state.activeThreadId;
    if (open && dock.activeId !== open && dock.results.some((r) => r.threadId === open)) {
      dispatch({ type: "searchDockActive", threadId: open });
    }
  }, [dock, state.activeThreadId, dispatch]);

  // Keep the active row in view as you step with the arrows.
  useEffect(() => {
    if (!dock?.activeId) return;
    const el = listRef.current?.querySelector<HTMLElement>(
      `[data-thread-id="${CSS.escape(dock.activeId)}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [dock?.activeId]);

  if (!dock) return null;

  // Results whose thread has since been deleted fall out of the list.
  const rows = dock.results
    .map((r) => ({ result: r, thread: findThread(state, r.threadId) }))
    .filter((row): row is { result: (typeof dock.results)[number]; thread: Thread } =>
      row.thread != null,
    );
  const activeIndex = rows.findIndex((row) => row.thread.id === dock.activeId);

  function projectName(id: string) {
    if (isQuickHomeProjectId(id)) return "Quick threads";
    if (id === HERMES_HOME_PROJECT_ID) return "Agents";
    return state.projects.find((p) => p.id === id)?.name ?? "Project";
  }

  function open(threadId: string) {
    dispatch({ type: "searchDockActive", threadId });
    void actions.selectThread(threadId);
  }

  function step(delta: number) {
    if (rows.length === 0) return;
    const next = activeIndex < 0 ? 0 : (activeIndex + delta + rows.length) % rows.length;
    open(rows[next].thread.id);
  }

  const modelLabel =
    SMART_SEARCH_MODELS.find((m) => m.id === dock.model)?.label ?? dock.model;

  return (
    <aside className="search-dock" aria-label="Search results">
      <div className="search-dock-head">
        <SearchIcon size={15} className="search-dock-glyph" />
        <div className="search-dock-title">
          <div className="search-dock-query" title={dock.query}>
            {dock.query}
          </div>
          <div className="search-dock-meta">
            {rows.length === 0
              ? "No results"
              : `${activeIndex >= 0 ? activeIndex + 1 : "–"} of ${rows.length}`}
            {" · "}
            {dock.rankedByModel ? `ranked by ${modelLabel}` : "keyword matches"}
          </div>
        </div>
        <button
          type="button"
          className="search-dock-btn"
          aria-label="Previous result"
          title="Previous result"
          disabled={rows.length < 2}
          onClick={() => step(-1)}
        >
          <ArrowUpIcon size={15} />
        </button>
        <button
          type="button"
          className="search-dock-btn"
          aria-label="Next result"
          title="Next result"
          disabled={rows.length < 2}
          onClick={() => step(1)}
        >
          <ArrowDownIcon size={15} />
        </button>
        <button
          type="button"
          className="search-dock-btn search-dock-close"
          aria-label="Close search results"
          title="Close results (keeps this chat open)"
          onClick={() => dispatch({ type: "searchDock", dock: null })}
        >
          <XIcon size={15} />
        </button>
      </div>
      <div className="search-dock-list" ref={listRef}>
        {rows.length === 0 ? (
          <div className="search-dock-empty">Nothing matched. Close this and search again.</div>
        ) : (
          rows.map(({ result, thread }) => (
            <button
              key={thread.id}
              type="button"
              data-thread-id={thread.id}
              className={`search-dock-row${thread.id === dock.activeId ? " active" : ""}`}
              onClick={() => open(thread.id)}
            >
              <div className="search-dock-row-top">
                <AgentMark agent={thread.agent} size={16} className="search-dock-mark" />
                <span className="search-dock-row-title">{thread.title || "Untitled thread"}</span>
                <span className="search-dock-row-time">{timeAgo(thread.updatedAt)}</span>
              </div>
              <div className="search-dock-row-project">{projectName(thread.projectId)}</div>
              {result.reason && <div className="search-dock-row-reason">{result.reason}</div>}
              {result.snippet && <div className="search-dock-row-snippet">{result.snippet}</div>}
            </button>
          ))
        )}
      </div>
    </aside>
  );
});
