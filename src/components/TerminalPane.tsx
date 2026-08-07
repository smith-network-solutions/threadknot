import { useEffect, useRef, useState } from "react";
import type { Project } from "../lib/protocol";
import { useStore } from "../state/store";
import { TerminalInstance } from "./terminal/TerminalInstance";
import "../styles/terminal.css";

/**
 * Interactive pty terminal panel: a tab strip over one xterm per session.
 * Tabs are project-bound and persisted server-side, so they survive an app
 * restart (with replayed scrollback) and can be renamed. The pane is keyed by
 * project.id in WorkspacePanel, so switching projects shows that project's own
 * terminals.
 */
export function TerminalPane({
  project,
  active,
  machineId,
}: {
  project: Project;
  active: boolean;
  /** Owning machine when the project lives on a peer — the pty runs there
   *  and this server splices the socket through. */
  machineId?: string;
}) {
  const { state, actions } = useStore();
  const http = state.http;
  const terminals = state.terminals[project.id]; // undefined until first load
  const [current, setCurrent] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const bootstrapped = useRef(false);
  const creating = useRef(false);

  // On first activation, load this project's terminals from the server.
  useEffect(() => {
    if (!active || bootstrapped.current) return;
    bootstrapped.current = true;
    void actions.refreshTerminals(project.id).catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  // Once loaded, ensure at least one terminal exists (fresh project → create).
  useEffect(() => {
    if (!active || terminals === undefined || creating.current) return;
    if (terminals.length === 0) {
      creating.current = true;
      actions
        .createTerminal(project.id)
        .then((t) => setCurrent(t.id))
        .catch(() => undefined)
        .finally(() => {
          creating.current = false;
        });
    }
  }, [active, terminals, project.id, actions]);

  // Keep the focused tab valid against the live list (e.g. after a delete).
  useEffect(() => {
    if (!terminals || terminals.length === 0) return;
    setCurrent((cur) => (cur && terminals.some((t) => t.id === cur) ? cur : terminals[0].id));
  }, [terminals]);

  async function addTab() {
    const t = await actions.createTerminal(project.id).catch(() => null);
    if (t) setCurrent(t.id);
  }

  function commitRename(id: string, value: string) {
    setEditing(null);
    const name = value.trim();
    const tab = (terminals ?? []).find((t) => t.id === id);
    if (name && tab && name !== tab.name) void actions.renameTerminal(id, name);
  }

  if (!http) {
    return <div className="terminal-pane ws-stub">Terminal unavailable — no server connection.</div>;
  }

  const tabs = terminals ?? [];

  return (
    <div className="terminal-pane">
      <div className="term-tabs" role="tablist">
        {tabs.map((t) => (
          <div key={t.id} className={`term-tab${t.id === current ? " on" : ""}`}>
            {editing === t.id ? (
              <input
                className="term-tab-edit"
                defaultValue={t.name}
                autoFocus
                onFocus={(e) => e.currentTarget.select()}
                onBlur={(e) => commitRename(t.id, e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitRename(t.id, e.currentTarget.value);
                  else if (e.key === "Escape") setEditing(null);
                }}
              />
            ) : (
              <button
                type="button"
                className="term-tab-label"
                role="tab"
                aria-selected={t.id === current}
                onClick={() => setCurrent(t.id)}
                onDoubleClick={() => setEditing(t.id)}
                title="Double-click to rename"
              >
                {t.name}
                {!t.alive && <span className="term-tab-dead" title="shell exited"> ·</span>}
              </button>
            )}
            <button
              type="button"
              className="term-tab-x"
              aria-label={`Delete ${t.name}`}
              title="Delete terminal (permanent)"
              onClick={() => void actions.deleteTerminal(t.id)}
            >
              ×
            </button>
          </div>
        ))}
        <button type="button" className="term-tab-add" aria-label="New terminal" onClick={addTab}>
          +
        </button>
      </div>
      <div className="term-stack">
        {tabs.map((t) => (
          <TerminalInstance
            key={t.id}
            project={project}
            termId={t.id}
            http={http}
            active={active && t.id === current}
            machineId={machineId}
          />
        ))}
      </div>
    </div>
  );
}
