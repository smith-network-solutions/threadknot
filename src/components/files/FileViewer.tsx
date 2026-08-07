import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import hljs from "highlight.js/lib/common";
import "highlight.js/styles/github-dark.css";
import type { FileReadData, Project } from "../../lib/protocol";
import { fileUrl } from "../../lib/discovery";
import { downloadViaShell } from "../../lib/download";
import { Markdown } from "../Markdown";
import { useStore } from "../../state/store";
import { ChevronIcon, DownloadIcon } from "../icons";
import { absoluteProjectPath, ViewerClipboardActions } from "./ViewerActions";

const PdfViewer = lazy(() =>
  import("./PdfViewer").then((module) => ({ default: module.PdfViewer })),
);

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "avif", "bmp"]);
/** Played inline rather than offered as a binary download. */
const VIDEO_EXTS = new Set(["mp4", "m4v", "webm", "mov", "ogv"]);
const MARKDOWN_EXTS = new Set(["md", "markdown"]);
const HTML_EXTS = new Set(["html", "htm", "xhtml"]);

/** Map a file extension to a highlight.js language id (only ids that exist in
 *  the `common` bundle). Anything unmapped falls back to auto-detection. */
const HLJS_LANG: Record<string, string> = {
  ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript",
  js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
  py: "python", pyi: "python",
  rs: "rust", go: "go", rb: "ruby", java: "java",
  kt: "kotlin", kts: "kotlin", swift: "swift",
  c: "c", h: "c", cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp",
  cs: "csharp", php: "php",
  sh: "bash", bash: "bash", zsh: "bash",
  sql: "sql", json: "json", jsonc: "json",
  yaml: "yaml", yml: "yaml",
  xml: "xml", html: "xml", htm: "xml", svg: "xml", vue: "xml", xhtml: "xml",
  css: "css", scss: "scss", less: "less",
  lua: "lua", r: "r", pl: "perl", pm: "perl",
  md: "markdown", markdown: "markdown",
  toml: "ini", ini: "ini", cfg: "ini", conf: "ini",
  diff: "diff", patch: "diff",
  graphql: "graphql", gql: "graphql",
};

function extOf(path: string): string {
  const base = path.slice(path.lastIndexOf("/") + 1);
  if (base.toLowerCase() === "makefile") return "makefile";
  if (base.toLowerCase() === "dockerfile") return "dockerfile";
  const dot = base.lastIndexOf(".");
  return dot <= 0 ? "" : base.slice(dot + 1).toLowerCase();
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** Highlighted, horizontally-scrollable code block. */
function CodeView({ contents, ext }: { contents: string; ext: string }) {
  const html = useMemo(() => {
    const lang = HLJS_LANG[ext];
    try {
      if (lang && hljs.getLanguage(lang)) {
        return hljs.highlight(contents, { language: lang, ignoreIllegals: true }).value;
      }
      return hljs.highlightAuto(contents).value;
    } catch {
      return escapeHtml(contents);
    }
  }, [contents, ext]);
  return (
    <pre className="files-code">
      <code className="hljs" dangerouslySetInnerHTML={{ __html: html }} />
    </pre>
  );
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

export function FileViewer({
  project,
  path,
  onBack,
  machineId,
}: {
  project: Project;
  path: string;
  onBack: () => void;
  /** Owning machine when the project lives on a peer (bytes stream through). */
  machineId?: string;
}) {
  const { state, actions } = useStore();
  const http = state.http;
  const ext = extOf(path);
  const isImage = IMAGE_EXTS.has(ext);
  const isVideo = VIDEO_EXTS.has(ext);
  const isPdf = ext === "pdf";
  const isMarkdown = MARKDOWN_EXTS.has(ext);
  const isHtml = HTML_EXTS.has(ext);
  const hasRenderedView = isMarkdown || isHtml;
  const absolutePath = absoluteProjectPath(project.path, path);

  const [data, setData] = useState<FileReadData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [rendered, setRendered] = useState(true);
  const dlRef = useRef<HTMLAnchorElement | null>(null);

  // Image/video/PDF render straight from /file — no need to pull the bytes over
  // WS, and for video that would mean buffering the whole thing just to decide
  // it is binary.
  const needsRead = !isImage && !isVideo && !isPdf;

  // Every newly opened Markdown/HTML file starts in its rendered view.
  useEffect(() => setRendered(true), [path]);

  useEffect(() => {
    if (!needsRead) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setData(null);
    actions
      .fsRead(project.id, path, machineId)
      .then((d) => {
        if (!cancelled) setData(d);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e?.message ?? e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [project.id, path, needsRead, actions]);

  const download = () => {
    if (!http) return;
    const href = fileUrl(http, project.id, path, { download: true, machineId });
    const suggested = path.slice(path.lastIndexOf("/") + 1) || path;
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

  return (
    <div className="files-viewer">
      <header className="files-viewer-head">
        <button type="button" className="files-back" onClick={onBack}>
          <ChevronIcon size={14} />
          <span>Files</span>
        </button>
        <span className="files-crumb" title={path}>
          {path}
        </span>
        <div className="files-viewer-actions">
          <ViewerClipboardActions
            absolutePath={absolutePath}
            fileTarget={{ kind: "project", projectId: project.id, path }}
          />
          <button
            type="button"
            className="files-action files-dl"
            onClick={download}
            disabled={!http}
            title="Download file"
          >
            <DownloadIcon size={13} />
            <span className="files-action-label">Download</span>
          </button>
        </div>
        <a ref={dlRef} hidden aria-hidden="true" download />
      </header>

      {hasRenderedView && data && !data.binary && (
        <div className="files-viewer-sub">
          <div className="files-seg">
            <button
              type="button"
              className={rendered ? "on" : ""}
              onClick={() => setRendered(true)}
            >
              Rendered
            </button>
            <button
              type="button"
              className={!rendered ? "on" : ""}
              onClick={() => setRendered(false)}
            >
              Source
            </button>
          </div>
        </div>
      )}

      {data?.truncated && (
        <div className="files-notice">
          Showing the first 1&nbsp;MiB of {formatBytes(data.byteLength)}. Download for the full file.
        </div>
      )}

      <div className="files-viewer-body">
        {loading && <div className="files-empty">Loading…</div>}
        {error && <div className="files-empty files-error">{error}</div>}

        {isImage && http && (
          <div className="files-media">
            <img src={fileUrl(http, project.id, path, { machineId })} alt={path} />
          </div>
        )}
        {isVideo && http && (
          <div className="files-media">
            <video
              src={fileUrl(http, project.id, path, { machineId })}
              controls
              playsInline
              preload="metadata"
            />
          </div>
        )}
        {isPdf && http && (
          <Suspense fallback={<div className="files-empty">Opening PDF viewer…</div>}>
            <PdfViewer src={fileUrl(http, project.id, path, { machineId })} title={path} />
          </Suspense>
        )}
        {(isImage || isVideo || isPdf) && !http && (
          <div className="files-empty">Server connection unavailable.</div>
        )}

        {needsRead && data && (
          data.binary ? (
            <div className="files-download-prompt">
              <p className="files-dp-title">Binary file</p>
              <p className="files-dp-sub">
                {ext ? `.${ext} · ` : ""}
                {formatBytes(data.byteLength)}
              </p>
              <button type="button" className="files-dl-big" onClick={download} disabled={!http}>
                Download
              </button>
            </div>
          ) : isMarkdown && rendered ? (
            <div className="files-md">
              <Markdown text={data.contents} />
            </div>
          ) : isHtml && rendered ? (
            <iframe
              className="files-html-preview"
              srcDoc={data.contents}
              sandbox=""
              referrerPolicy="no-referrer"
              title={`Rendered preview of ${path}`}
            />
          ) : (
            <CodeView contents={data.contents} ext={ext} />
          )
        )}
      </div>
    </div>
  );
}
