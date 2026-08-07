import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ArtifactRecord, Project } from "../lib/protocol";
import { useStore } from "../state/store";
import { artifactFileUrl } from "../lib/discovery";
import { downloadViaShell } from "../lib/download";
import { timeAgo } from "../lib/format";
import { Markdown } from "./Markdown";
import { ChevronIcon, DownloadIcon, FileIcon, TrashIcon } from "./icons";
import { absoluteProjectPath, ViewerClipboardActions } from "./files/ViewerActions";
import "../styles/files.css";

const PdfViewer = lazy(() =>
  import("./files/PdfViewer").then((module) => ({ default: module.PdfViewer })),
);

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "avif", "bmp"]);
const MARKDOWN_EXTS = new Set(["md", "markdown"]);
/** Playable inline. Recorded walkthroughs (`browser_record_flow`) land here, and
 *  a training video the user has to download first is a training video nobody
 *  watches. */
const VIDEO_EXTS = new Set(["mp4", "m4v", "webm", "mov", "ogv"]);

function extOf(path: string): string {
  const base = path.slice(path.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  return dot <= 0 ? "" : base.slice(dot + 1).toLowerCase();
}

/** The artifact's real file extension.
 *
 *  Deliberately NOT taken from `name`: for a published artifact that is the
 *  agent-supplied title ("BTC-USD price — Yahoo Finance"), which usually has no
 *  extension — and a title like "report v1.2" would parse as ext "2". Typing a
 *  PNG off its title sent it down the "can't preview this" path and offered it
 *  as an extensionless download. `relPath` is the file we actually stored. */
function artifactExt(artifact: ArtifactRecord): string {
  const ext = extOf(artifact.relPath);
  if (ext) return ext;
  const sub = artifact.mimeType.split(";")[0].split("/")[1] ?? "";
  return sub === "jpeg" ? "jpg" : sub.toLowerCase();
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** Preview + download one artifact from its durable snapshot. */
function ArtifactViewer({
  artifact,
  project,
  onBack,
  machineId,
}: {
  artifact: ArtifactRecord;
  project: Project;
  onBack: () => void;
  machineId?: string;
}) {
  const { state, actions } = useStore();
  const http = state.http;
  const ext = artifactExt(artifact);
  const isImage = IMAGE_EXTS.has(ext) || artifact.mimeType.startsWith("image/");
  const isVideo = VIDEO_EXTS.has(ext) || artifact.mimeType.startsWith("video/");
  const isPdf = ext === "pdf" || artifact.mimeType === "application/pdf";
  const isMarkdown = MARKDOWN_EXTS.has(ext);
  const textLike =
    !isImage &&
    !isVideo &&
    !isPdf &&
    (/text|json|csv|xml|html|svg/.test(artifact.mimeType) ||
      ["txt", "csv", "tsv", "json", "html", "htm", "md", "markdown"].includes(ext));

  const url = http ? artifactFileUrl(http, artifact.id, { machineId }) : null;
  const absolutePath = absoluteProjectPath(project.path, artifact.relPath);
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const dlRef = useRef<HTMLAnchorElement | null>(null);

  useEffect(() => {
    if (!deleteArmed) return;
    const timeout = window.setTimeout(() => setDeleteArmed(false), 2600);
    return () => window.clearTimeout(timeout);
  }, [deleteArmed]);

  useEffect(() => {
    if (!url || isImage || isVideo || isPdf || !textLike) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setText(null);
    fetch(url)
      .then((r) => (r.ok ? r.text() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then((t) => !cancelled && setText(t))
      .catch((e) => !cancelled && setError(String(e?.message ?? e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [url, isImage, isVideo, isPdf, textLike]);

  const download = () => {
    if (!http) return;
    const href = artifactFileUrl(http, artifact.id, { download: true, machineId });
    // Prefer relPath's basename: it carries the real extension, whereas `name`
    // may be an extensionless display title. See `artifactExt`.
    const suggested = artifact.relPath.slice(artifact.relPath.lastIndexOf("/") + 1) || artifact.name;
    void downloadViaShell(href, suggested)
      .then((handled) => {
        if (handled) return;
        const a = dlRef.current;
        if (!a) return;
        a.href = href;
        a.click();
      })
      .catch((e) => setError(String((e as Error)?.message ?? e)));
  };

  const deleteArtifact = async () => {
    if (!deleteArmed) {
      setDeleteArmed(true);
      return;
    }
    setDeleting(true);
    setError(null);
    try {
      await actions.deleteArtifact(artifact.id);
      onBack();
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
      setDeleting(false);
      setDeleteArmed(false);
    }
  };

  return (
    <div className="files-viewer">
      <header className="files-viewer-head">
        <button type="button" className="files-back" onClick={onBack}>
          <ChevronIcon size={14} />
          <span>Artifacts</span>
        </button>
        <span className="files-crumb" title={artifact.relPath}>
          {artifact.name}
        </span>
        <div className="files-viewer-actions">
          <ViewerClipboardActions
            absolutePath={absolutePath}
            fileTarget={{ kind: "artifact", artifactId: artifact.id }}
          />
          <button
            type="button"
            className="files-action files-dl"
            onClick={download}
            disabled={!http}
            title="Download artifact"
          >
            <DownloadIcon size={13} />
            <span className="files-action-label">Download</span>
          </button>
          <button
            type="button"
            className={`files-action files-delete${deleteArmed ? " armed" : ""}`}
            onClick={() => void deleteArtifact()}
            disabled={deleting}
            title={deleteArmed ? "Click again to permanently delete" : "Delete artifact"}
            aria-label={deleteArmed ? "Confirm delete artifact" : "Delete artifact"}
          >
            <TrashIcon size={13} />
            <span className="files-action-label">
              {deleting ? "Deleting…" : deleteArmed ? "Confirm" : "Delete"}
            </span>
          </button>
        </div>
        <a ref={dlRef} hidden aria-hidden="true" download />
      </header>

      <div className="files-viewer-body">
        {!http && <div className="files-empty">Server connection unavailable.</div>}
        {loading && <div className="files-empty">Loading…</div>}
        {error && <div className="files-empty files-error">{error}</div>}

        {url && isImage && (
          <div className="files-media">
            <img src={url} alt={artifact.name} />
          </div>
        )}
        {url && isVideo && (
          <div className="files-media">
            {/* No autoplay: a walkthrough should start when the viewer decides
                to watch it, not when they happen to open the tab. */}
            <video src={url} controls playsInline preload="metadata" />
          </div>
        )}
        {url && isPdf && (
          <Suspense fallback={<div className="files-empty">Opening PDF viewer…</div>}>
            <PdfViewer src={url} title={artifact.name} />
          </Suspense>
        )}
        {text != null && isMarkdown && (
          <div className="files-md">
            <Markdown text={text} />
          </div>
        )}
        {text != null && !isMarkdown && <pre className="files-code">{text}</pre>}

        {url && !isImage && !isVideo && !isPdf && !textLike && (
          <div className="files-download-prompt">
            <p className="files-dp-title">{ext ? `.${ext} file` : "File"}</p>
            <p className="files-dp-sub">{formatBytes(artifact.sizeBytes)}</p>
            <button type="button" className="files-dl-big" onClick={download}>
              Download
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** Produced-artifacts list for a project (agent outputs: PDFs, images, reports).
 *  Scoped to the whole project, with a toggle to show only the current chat. */
export function ArtifactsPane({
  project,
  active,
  machineId,
}: {
  project: Project;
  active: boolean;
  /** Owning machine when the project lives on a peer (bytes stream through). */
  machineId?: string;
}) {
  const { state, actions, dispatch } = useStore();
  const all = state.artifacts[project.id];
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [thisChatOnly, setThisChatOnly] = useState(false);
  const [deleteArmedId, setDeleteArmedId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const loadedFor = useRef<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await actions.listArtifacts(project.id);
    } catch (e) {
      setError(String((e as Error)?.message ?? e));
    } finally {
      setLoading(false);
    }
  }, [actions, project.id]);

  // Lazy-init on first `active`; refetch when the project changes.
  useEffect(() => {
    if (!active) return;
    if (loadedFor.current === project.id) return;
    loadedFor.current = project.id;
    setSelectedId(null);
    void load();
  }, [active, project.id, load]);

  // Deep link from a chat artifact card: open that artifact in the viewer.
  // A just-produced artifact may not be in the loaded list yet — refetch.
  const focusId = state.artifactFocus;
  useEffect(() => {
    if (!active || !focusId) return;
    dispatch({ type: "artifactFocus", artifactId: null });
    setSelectedId(focusId);
    if (!(state.artifacts[project.id] ?? []).some((a) => a.id === focusId)) {
      void load();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, focusId, project.id, load]);

  useEffect(() => {
    if (!deleteArmedId) return;
    const timeout = window.setTimeout(() => setDeleteArmedId(null), 2600);
    return () => window.clearTimeout(timeout);
  }, [deleteArmedId]);

  const deleteFromList = useCallback(
    async (artifact: ArtifactRecord) => {
      if (deleteArmedId !== artifact.id) {
        setDeleteArmedId(artifact.id);
        return;
      }
      setDeletingId(artifact.id);
      setError(null);
      try {
        await actions.deleteArtifact(artifact.id);
        // Do not make the user wait for the state.changed round trip before
        // the deleted row disappears. The broadcast still updates other clients.
        await actions.listArtifacts(project.id);
        setDeleteArmedId(null);
      } catch (e) {
        setError(String((e as Error)?.message ?? e));
        setDeleteArmedId(null);
      } finally {
        setDeletingId(null);
      }
    },
    [actions, deleteArmedId, project.id],
  );

  const shown = useMemo(() => {
    const list = all ?? [];
    if (thisChatOnly && state.activeThreadId) {
      return list.filter((a) => a.threadId === state.activeThreadId);
    }
    return list;
  }, [all, thisChatOnly, state.activeThreadId]);

  const selected = selectedId ? (all ?? []).find((a) => a.id === selectedId) ?? null : null;
  if (selected) {
    return (
      <ArtifactViewer artifact={selected} project={project} machineId={machineId} onBack={() => setSelectedId(null)} />
    );
  }

  return (
    <div className="files-pane">
      <header className="files-head">
        <div className="files-head-top">
          <span className="files-count">
            {loading && all == null
              ? "Loading…"
              : `${shown.length} artifact${shown.length === 1 ? "" : "s"}`}
          </span>
          {state.activeThreadId && (
            <button
              type="button"
              className={`files-refresh${thisChatOnly ? " on" : ""}`}
              onClick={() => setThisChatOnly((v) => !v)}
              title="Show only artifacts produced in the current chat"
            >
              {thisChatOnly ? "This chat" : "All chats"}
            </button>
          )}
          <button
            type="button"
            className="files-refresh"
            onClick={() => void load()}
            disabled={loading}
            title="Refresh"
          >
            Refresh
          </button>
        </div>
      </header>

      <div className="files-tree">
        {error && <div className="files-empty files-error">{error}</div>}
        {!error && all != null && shown.length === 0 && (
          <div className="files-empty">
            No artifacts yet. Files the agent produces (PDFs, images, reports, data) show up here.
          </div>
        )}
        {!error &&
          shown.map((a) => {
            const armed = deleteArmedId === a.id;
            const deleting = deletingId === a.id;
            return (
              <div key={a.id} className="artifact-list-row" role="group" aria-label={a.name}>
                <button
                  type="button"
                  className="files-row files-file artifact-list-open"
                  onClick={() => setSelectedId(a.id)}
                  title={a.relPath}
                >
                  <span className="files-chevron-spacer" />
                  <FileIcon size={14} />
                  <span className="files-name" title={a.description ?? undefined}>
                    {a.name}
                  </span>
                  <span className="files-size">
                    {a.op === "modified" ? "edited" : "new"} · {formatBytes(a.sizeBytes)} ·{" "}
                    {timeAgo(a.createdAt)}
                  </span>
                </button>
                <button
                  type="button"
                  className={`artifact-list-delete${armed ? " armed" : ""}`}
                  onClick={() => void deleteFromList(a)}
                  disabled={deleting}
                  title={armed ? `Click again to delete ${a.name}` : `Delete ${a.name}`}
                  aria-label={armed ? `Confirm delete ${a.name}` : `Delete ${a.name}`}
                >
                  <TrashIcon size={13} />
                  {armed && <span>{deleting ? "Deleting…" : "Delete?"}</span>}
                </button>
              </div>
            );
          })}
      </div>
    </div>
  );
}
