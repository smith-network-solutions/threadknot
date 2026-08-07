import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { findThread, resolveProjectView, useStore } from "../state/store";
import { ThreadView } from "./ThreadView";
import { WorkspacePanel, type WorkspaceOrient } from "./WorkspacePanel";
import "../styles/main-split.css";

const LS_ORIENT = "threadknot.ws.orient";
const LS_FRAC = "threadknot.ws.frac";
const DEFAULT_FRAC = 0.42;
const MIN_FRAC = 0.18;
const MAX_FRAC = 0.82;

function loadOrient(): WorkspaceOrient {
  return localStorage.getItem(LS_ORIENT) === "stacked" ? "stacked" : "side";
}

function loadFrac(): number {
  const v = parseFloat(localStorage.getItem(LS_FRAC) ?? "");
  return Number.isFinite(v) && v >= MIN_FRAC && v <= MAX_FRAC ? v : DEFAULT_FRAC;
}

/**
 * Wraps the chat pane and the workspace panel so their shared axis can be both
 * resized (drag the divider) and re-oriented (side-by-side vs. stacked). The
 * divider and sizing only apply on desktop; on mobile the workspace is a
 * full-screen overlay (see workspace.css) and these controls are hidden.
 *
 * `--ws-frac` is the workspace panel's fraction of the split (width when
 * `side`, height when `stacked`). Persisted to localStorage.
 */
export function MainSplit() {
  const { state } = useStore();

  // Mirror WorkspacePanel's own visibility guard so the divider only shows when
  // the panel is actually rendered.
  const thread = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
  const projectId = thread?.projectId ?? state.draft?.projectId ?? null;
  const project = resolveProjectView(state, projectId)?.project ?? null;
  const workspaceOpen = project != null && state.workspace[project.id] != null;

  const [orient, setOrient] = useState<WorkspaceOrient>(loadOrient);
  const [frac, setFrac] = useState<number>(loadFrac);
  const [dragging, setDragging] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    localStorage.setItem(LS_ORIENT, orient);
  }, [orient]);
  useEffect(() => {
    localStorage.setItem(LS_FRAC, String(frac));
  }, [frac]);

  useEffect(() => {
    if (!dragging) return;
    function move(e: PointerEvent) {
      const el = ref.current;
      if (!el) return;
      // Nothing above or around this element is zoomed any more (the interface
      // zoom lives on the message feed alone), so the rect and clientX/clientY
      // are already in the same true viewport space. The fraction below was a
      // ratio of the two either way, so it needed no correction before and
      // needs none now.
      const r = el.getBoundingClientRect();
      const raw =
        orient === "side"
          ? (r.right - e.clientX) / r.width
          : (r.bottom - e.clientY) / r.height;
      setFrac(Math.min(MAX_FRAC, Math.max(MIN_FRAC, raw)));
    }
    function up() {
      setDragging(false);
    }
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
  }, [dragging, orient]);

  function onDividerDown(e: ReactPointerEvent) {
    e.preventDefault();
    setDragging(true);
  }

  const style = { ["--ws-frac" as string]: String(frac) } as React.CSSProperties;

  return (
    <div
      ref={ref}
      className={`main-split ${orient}${workspaceOpen ? " ws-open" : ""}${dragging ? " dragging" : ""}`}
      style={style}
    >
      <ThreadView />
      {workspaceOpen && (
        <div
          className="ws-divider"
          role="separator"
          aria-orientation={orient === "side" ? "vertical" : "horizontal"}
          aria-label="Resize workspace panel"
          onPointerDown={onDividerDown}
          onDoubleClick={() => setFrac(DEFAULT_FRAC)}
        />
      )}
      <WorkspacePanel
        orient={orient}
        onToggleOrient={() => setOrient((o) => (o === "side" ? "stacked" : "side"))}
      />
      {dragging && (
        <div
          className={`ws-drag-mask ${orient}`}
          aria-hidden="true"
        />
      )}
    </div>
  );
}
