import { useEffect, useRef } from "react";
import { findThread, resolveProjectView, useStore, type WorkspaceTab } from "../state/store";
import { FilesPane } from "./FilesPane";
import { GitPane } from "./GitPane";
import { ArtifactsPane } from "./ArtifactsPane";
import { BrowserPane } from "./BrowserPane";
import { TerminalPane } from "./TerminalPane";
import {
  ArchiveIcon,
  FolderIcon,
  GitBranchIcon,
  GlobeIcon,
  LayoutSideIcon,
  LayoutStackedIcon,
  TerminalIcon,
  XIcon,
} from "./icons";
import "../styles/workspace.css";

export type WorkspaceOrient = "side" | "stacked";

export const WORKSPACE_TABS = [
  { id: "files", label: "Files", Icon: FolderIcon },
  { id: "git", label: "Git", Icon: GitBranchIcon },
  { id: "artifacts", label: "Artifacts", Icon: ArchiveIcon },
  { id: "browser", label: "Browser", Icon: GlobeIcon },
  { id: "terminal", label: "Terminal", Icon: TerminalIcon },
] as const;

/** Tabs actually shown in the panel strip AND the ThreadView header toggles. */
export const VISIBLE_TABS = WORKSPACE_TABS;

/**
 * Right-side workspace panel (full-screen overlay on mobile) hosting all
 * project workspace tabs. Panes stay mounted while the panel is open so
 * terminal buffers and browser state survive tab switches; each pane must
 * lazy-init on first `active`.
 */
export function WorkspacePanel({
  orient,
  onToggleOrient,
}: {
  orient?: WorkspaceOrient;
  onToggleOrient?: () => void;
}) {
  const { state, dispatch } = useStore();
  const thread = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
  const projectId = thread?.projectId ?? state.draft?.projectId ?? null;
  // Local record, or a view synthesized from workspace membership for a
  // root on another machine — panes then route/stream through the owner.
  const view = resolveProjectView(state, projectId);
  const project = view?.project ?? null;
  const machineId = view?.machineId;
  const tab = project ? state.workspace[project.id] ?? null : null;

  // The strip scrolls when the tabs outrun the panel width, so the selected tab
  // can sit off-screen when the switch came from elsewhere (ThreadView's header
  // toggles) — pull it into view. `nearest` keeps the page itself from moving.
  const tabsRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const on = tabsRef.current?.querySelector(".ws-tab.on");
    on?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [tab]);

  if (!tab || !project) return null;

  return (
    <aside className="workspace-panel">
      <header className="workspace-head">
        <nav className="workspace-tabs" ref={tabsRef}>
          {VISIBLE_TABS.map(({ id, label, Icon }) => (
            <button
              key={id}
              type="button"
              className={`ws-tab${tab === id ? " on" : ""}`}
              onClick={() =>
                dispatch({ type: "workspace", projectId: project.id, tab: id as WorkspaceTab })
              }
            >
              <Icon size={14} />
              <span>{label}</span>
            </button>
          ))}
        </nav>
        {onToggleOrient && (
          <button
            type="button"
            className="icon-btn ws-orient"
            aria-label={orient === "stacked" ? "Place panel beside chat" : "Stack panel below chat"}
            title={orient === "stacked" ? "Place beside chat" : "Stack below chat"}
            onClick={onToggleOrient}
          >
            {orient === "stacked" ? <LayoutSideIcon size={16} /> : <LayoutStackedIcon size={16} />}
          </button>
        )}
        <button
          type="button"
          className="icon-btn ws-close"
          aria-label="Close panel"
          onClick={() => dispatch({ type: "workspace", projectId: project.id, tab: null })}
        >
          <XIcon size={16} />
        </button>
      </header>
      <div className="workspace-body">
        <div className="ws-pane" hidden={tab !== "files"}>
          <FilesPane project={project} active={tab === "files"} machineId={machineId} />
        </div>
        <div className="ws-pane" hidden={tab !== "git"}>
          <GitPane project={project} active={tab === "git"} />
        </div>
        <div className="ws-pane" hidden={tab !== "artifacts"}>
          <ArtifactsPane
            project={project}
            active={tab === "artifacts"}
            machineId={machineId}
          />
        </div>
        <div className="ws-pane" hidden={tab !== "browser"}>
          {/* Chrome runs on the workspace's owning machine; for a remote
              workspace this server splices the socket through to it, so the
              pane drives that machine's browser and its stored logins. */}
          <BrowserPane
            key={state.activeThreadId ?? `project:${project.id}`}
            project={project}
            active={tab === "browser"}
            sessionId={state.activeThreadId ?? `project:${project.id}`}
            machineId={machineId}
          />
        </div>
        <div className="ws-pane" hidden={tab !== "terminal"}>
          <TerminalPane
            key={project.id}
            project={project}
            active={tab === "terminal"}
            machineId={machineId}
          />
        </div>
      </div>
    </aside>
  );
}
