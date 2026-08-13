import { startTransition, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Project, TreeEntry } from "../lib/protocol";
import { useStore } from "../state/store";
import { buildTree, filterTree, type TreeNode } from "./files/tree";
import { FileViewer } from "./files/FileViewer";
import { ChevronIcon, FileIcon, FolderIcon, SearchIcon } from "./icons";
import "../styles/files.css";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** One tree row (recursive). Directories toggle; files open the viewer. */
function Row({
  node,
  depth,
  visible,
  expanded,
  toggle,
  onOpen,
}: {
  node: TreeNode;
  depth: number;
  /** Pre-computed filter result; null = no active query (show everything). */
  visible: Set<string> | null;
  expanded: Set<string>;
  toggle: (path: string) => void;
  onOpen: (path: string) => void;
}) {
  if (visible && !visible.has(node.path)) return null;
  const pad = { paddingLeft: `${8 + depth * 14}px` };

  if (node.kind === "dir") {
    const open = visible ? true : expanded.has(node.path);
    return (
      <>
        <button
          type="button"
          className="files-row files-dir"
          style={pad}
          onClick={() => toggle(node.path)}
        >
          <span className={`files-chevron${open ? " open" : ""}`}>
            <ChevronIcon size={12} />
          </span>
          <FolderIcon size={14} />
          <span className="files-name">{node.name}</span>
        </button>
        {open &&
          node.children?.map((c) => (
            <Row
              key={c.path}
              node={c}
              depth={depth + 1}
              visible={visible}
              expanded={expanded}
              toggle={toggle}
              onOpen={onOpen}
            />
          ))}
      </>
    );
  }

  return (
    <button type="button" className="files-row files-file" style={pad} onClick={() => onOpen(node.path)}>
      <span className="files-chevron-spacer" />
      <FileIcon size={14} />
      <span className="files-name">{node.name}</span>
      {node.sizeBytes != null && <span className="files-size">{formatBytes(node.sizeBytes)}</span>}
    </button>
  );
}

/** Project file tree + viewer. Mobile-first: tree and viewer are stacked
 *  (viewer replaces the tree) rather than side-by-side. */
export function FilesPane({
  project,
  active,
  machineId,
}: {
  project: Project;
  active: boolean;
  /** Owning machine when the project lives on a peer (routes fs traffic). */
  machineId?: string;
}) {
  const { state, dispatch, actions } = useStore();
  const [entries, setEntries] = useState<TreeEntry[] | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  /** Debounced, normalized version of `filter` that actually drives filtering. */
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(null);
  const filterInputRef = useRef<HTMLInputElement>(null);
  const loadedFor = useRef<string | null>(null);

  // Files owns Ctrl/Cmd+F while the tree is visible. FileViewer registers its
  // own handler when a file is open, so the same shortcut can find within the
  // document instead of unexpectedly returning to the tree filter.
  useEffect(() => {
    if (!active || selected) return;
    function onFind(event: KeyboardEvent) {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "f") return;
      if (!(event.target as HTMLElement | null)?.closest(".workspace-panel")) return;
      event.preventDefault();
      filterInputRef.current?.focus();
      filterInputRef.current?.select();
    }
    window.addEventListener("keydown", onFind);
    return () => window.removeEventListener("keydown", onFind);
  }, [active, selected]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await actions.fsTree(project.id, machineId);
      setEntries(data.entries);
      setTruncated(data.truncated);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [actions, project.id]);

  // Lazy-init on first `active`; refetch when the project changes.
  useEffect(() => {
    if (!active) return;
    if (loadedFor.current === project.id) return;
    loadedFor.current = project.id;
    setSelected(null);
    setFilter("");
    setQuery("");
    setExpanded(new Set());
    void load();
  }, [active, project.id, load]);

  // Chat citations can name either the project-relative path or a unique
  // basename. Resolve only unique basenames so a common name in two folders
  // never opens the wrong file silently.
  useEffect(() => {
    const focus = state.fileFocus;
    if (!active || !focus || focus.projectId !== project.id || entries === null) return;
    let requested = focus.path.replace(/^\.\//, "");
    try {
      requested = decodeURIComponent(requested);
    } catch {
      // Keep the original path when a model produced an incomplete escape.
    }
    const projectRoot = project.path.replace(/[\\/]$/, "");
    if (requested.startsWith(`${projectRoot}/`)) {
      requested = requested.slice(projectRoot.length + 1);
    }
    requested = requested.replace(/^\/+/, "");
    const files = entries.filter((entry) => entry.kind === "file");
    const exact = files.find((entry) => entry.path === requested);
    const basename = requested.slice(requested.lastIndexOf("/") + 1);
    const matches = files.filter(
      (entry) => entry.path.slice(entry.path.lastIndexOf("/") + 1) === basename,
    );
    const target = exact ?? (matches.length === 1 ? matches[0] : undefined);
    if (target) setSelected(target.path);
    dispatch({ type: "fileFocusClear", requestId: focus.requestId });
  }, [active, dispatch, entries, project.id, state.fileFocus]);

  // Debounce keystrokes into `query`, and apply it in a transition so the
  // input never blocks on re-rendering a big tree.
  useEffect(() => {
    const q = filter.trim().toLowerCase();
    if (q === query) return;
    const t = setTimeout(() => startTransition(() => setQuery(q)), 150);
    return () => clearTimeout(t);
  }, [filter, query]);

  const refresh = () => {
    loadedFor.current = project.id;
    void load();
  };

  const tree = useMemo(() => (entries ? buildTree(entries) : []), [entries]);
  const fileCount = useMemo(
    () => (entries ? entries.filter((e) => e.kind === "file").length : 0),
    [entries],
  );

  /** Cap on rendered matches — a broad query on a 25k tree stays snappy. */
  const MATCH_CAP = 300;
  const filtered = useMemo(
    () => (query ? filterTree(tree, query, MATCH_CAP) : null),
    [tree, query],
  );
  const filterPending = filter.trim().toLowerCase() !== query;

  const toggle = (path: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  if (selected) {
    return (
      <FileViewer
        project={project}
        path={selected}
        machineId={machineId}
        onBack={() => setSelected(null)}
      />
    );
  }

  return (
    <div className="files-pane">
      <header className="files-head">
        <div className="files-head-top">
          <span className="files-count">
            {loading && entries === null
              ? "Loading…"
              : filterPending
                ? "Filtering…"
                : filtered
                  ? `${filtered.total} match${filtered.total === 1 ? "" : "es"}`
                  : `${fileCount} file${fileCount === 1 ? "" : "s"}`}
          </span>
          {truncated && <span className="files-partial" title="Tree was capped at 25,000 entries">partial</span>}
          <button type="button" className="files-refresh" onClick={refresh} disabled={loading} title="Refresh">
            Refresh
          </button>
        </div>
        <label className="files-filter">
          <SearchIcon size={13} />
          <input
            ref={filterInputRef}
            type="text"
            placeholder="Filter files…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
          />
          {filter && (
            <button type="button" className="files-filter-clear" onClick={() => setFilter("")} aria-label="Clear filter">
              ×
            </button>
          )}
        </label>
      </header>

      <div className={`files-tree${filterPending ? " filtering" : ""}`}>
        {error && <div className="files-empty files-error">{error}</div>}
        {!error && entries !== null && tree.length === 0 && (
          <div className="files-empty">No files.</div>
        )}
        {!error && filtered && filtered.total === 0 && tree.length > 0 && (
          <div className="files-empty">No matches.</div>
        )}
        {!error &&
          tree.map((node) => (
            <Row
              key={node.path}
              node={node}
              depth={0}
              visible={filtered ? filtered.visible : null}
              expanded={expanded}
              toggle={toggle}
              onOpen={setSelected}
            />
          ))}
        {!error && filtered && filtered.total > filtered.shown && (
          <div className="files-more">
            Showing first {filtered.shown} of {filtered.total} matches — refine the filter
          </div>
        )}
      </div>
    </div>
  );
}
