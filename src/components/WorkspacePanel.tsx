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
  present,
  open,
  orient,
  onToggleOrient,
}: {
  present: boolean;
  open: boolean;
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
  const lastPanel = useRef<{
    project: NonNullable<typeof project>;
    machineId: typeof machineId;
    tab: WorkspaceTab;
  } | null>(null);
  if (project && tab) lastPanel.current = { project, machineId, tab };
  const panel = project && tab ? { project, machineId, tab } : lastPanel.current;
  const renderTab = panel?.tab ?? null;

  // The strip scrolls when the tabs outrun the panel width, so the selected tab
  // can sit off-screen when the switch came from elsewhere (ThreadView's header
  // toggles) — pull it into view. `nearest` keeps the page itself from moving.
  const panelRef = useRef<HTMLElement | null>(null);
  const tabsRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    const el = panelRef.current;
    if (!el) return;
    if (open) el.removeAttribute("inert");
    else el.setAttribute("inert", "");
  }, [open]);
  useEffect(() => {
    const on = tabsRef.current?.querySelector(".ws-tab.on");
    on?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [renderTab]);

  if (!present || !panel || !renderTab) return null;

  return (
    <aside
      ref={panelRef}
      className="workspace-panel"
      aria-hidden={!open || undefined}
    >
      <header className="workspace-head">
        <nav className="workspace-tabs" ref={tabsRef}>
          {VISIBLE_TABS.map(({ id, label, Icon }) => (
            <button
              key={id}
              type="button"
              className={`ws-tab${renderTab === id ? " on" : ""}`}
              onClick={() =>
                dispatch({ type: "workspace", projectId: panel.project.id, tab: id as WorkspaceTab })
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
          onClick={() => dispatch({ type: "workspace", projectId: panel.project.id, tab: null })}
        >
          <XIcon size={16} />
        </button>
      </header>
      <div className="workspace-body">
        <div className="ws-pane" data-zoom-pane="files" hidden={renderTab !== "files"}>
          <FilesPane
            project={panel.project}
            active={open && renderTab === "files"}
            machineId={panel.machineId}
          />
        </div>
        <div className="ws-pane" data-zoom-pane="git" hidden={renderTab !== "git"}>
          <GitPane project={panel.project} active={open && renderTab === "git"} />
        </div>
        <div className="ws-pane" data-zoom-pane="artifacts" hidden={renderTab !== "artifacts"}>
          <ArtifactsPane
            project={panel.project}
            active={open && renderTab === "artifacts"}
            machineId={panel.machineId}
          />
        </div>
        <div className="ws-pane" data-zoom-pane="browser" hidden={renderTab !== "browser"}>
          {/* Chrome runs on the workspace's owning machine; for a remote
              workspace this server splices the socket through to it, so the
              pane drives that machine's browser and its stored logins. */}
          <BrowserPane
            key={state.activeThreadId ?? `project:${panel.project.id}`}
            project={panel.project}
            active={open && renderTab === "browser"}
            sessionId={state.activeThreadId ?? `project:${panel.project.id}`}
            machineId={panel.machineId}
          />
        </div>
        <div className="ws-pane" hidden={renderTab !== "terminal"}>
          <TerminalPane
            key={panel.project.id}
            project={panel.project}
            active={open && renderTab === "terminal"}
            machineId={panel.machineId}
          />
        </div>
      </div>
    </aside>
  );
}
