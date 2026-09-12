import { HtmlPreview } from "./HtmlPreview";
import {
  lazy,
  Suspense,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import type { ArtifactRecord } from "../../lib/protocol";
import { Markdown } from "../Markdown";
import { FileIcon, PlayIcon } from "../icons";

const PdfViewer = lazy(() =>
  import("../files/PdfViewer").then((module) => ({ default: module.PdfViewer })),
);
const PdfPreview = lazy(() =>
  import("../files/PdfViewer").then((module) => ({ default: module.PdfPreview })),
);

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "avif", "bmp"]);
const VIDEO_EXTS = new Set(["mp4", "m4v", "webm", "mov", "ogv"]);
const MARKDOWN_EXTS = new Set(["md", "markdown", "mdx"]);
const HTML_EXTS = new Set(["html", "htm", "xhtml"]);
const DOCUMENT_EXTS = new Set(["doc", "docx", "odt", "rtf", "pages"]);
const SHEET_EXTS = new Set(["xls", "xlsx", "xlsm", "ods", "numbers"]);
const SLIDE_EXTS = new Set(["ppt", "pptx", "odp", "key"]);

export type ArtifactPreviewRecord = Pick<
  ArtifactRecord,
  "id" | "name" | "relPath" | "mimeType" | "sizeBytes" | "description" | "op"
>;

export type ArtifactKind =
  | "image"
  | "video"
  | "pdf"
  | "markdown"
  | "html"
  | "text"
  | "document"
  | "sheet"
  | "slides"
  | "archive"
  | "file";

export function artifactExtension(artifact: Pick<ArtifactRecord, "relPath" | "mimeType">): string {
  const base = artifact.relPath.slice(artifact.relPath.lastIndexOf("/") + 1);
  const dot = base.lastIndexOf(".");
  if (dot > 0) return base.slice(dot + 1).toLowerCase();
  const subtype = artifact.mimeType.split(";")[0].split("/")[1] ?? "";
  return subtype === "jpeg" ? "jpg" : subtype.toLowerCase();
}

export function artifactKind(artifact: ArtifactPreviewRecord): ArtifactKind {
  const ext = artifactExtension(artifact);
  const mime = artifact.mimeType.toLowerCase();
  if (IMAGE_EXTS.has(ext) || mime.startsWith("image/")) return "image";
  if (VIDEO_EXTS.has(ext) || mime.startsWith("video/")) return "video";
  if (ext === "pdf" || mime === "application/pdf") return "pdf";
  if (MARKDOWN_EXTS.has(ext)) return "markdown";
  if (HTML_EXTS.has(ext)) return "html";
  if (DOCUMENT_EXTS.has(ext) || /wordprocessingml|msword|opendocument\.text/.test(mime)) return "document";
  if (SHEET_EXTS.has(ext) || /spreadsheetml|ms-excel|opendocument\.spreadsheet/.test(mime)) return "sheet";
  if (SLIDE_EXTS.has(ext) || /presentationml|ms-powerpoint|opendocument\.presentation/.test(mime)) return "slides";
  if (/zip|gzip|compressed|archive|x-7z|x-rar/.test(mime) || ["zip", "gz", "tgz", "7z", "rar", "tar"].includes(ext)) return "archive";
  if (/text|json|csv|xml|javascript|typescript|yaml/.test(mime) || ["txt", "csv", "tsv", "json", "xml", "yaml", "yml", "toml", "log"].includes(ext)) return "text";
  return "file";
}

export function artifactTypeLabel(artifact: ArtifactPreviewRecord): string {
  const ext = artifactExtension(artifact);
  if (ext) return ext.toUpperCase();
  const kind = artifactKind(artifact);
  return kind === "file" ? "FILE" : kind.toUpperCase();
}

function DocumentPlaceholder({ artifact, kind }: { artifact: ArtifactPreviewRecord; kind: ArtifactKind }) {
  const label = artifactTypeLabel(artifact);
  return (
    <div className={`artifact-document-sheet kind-${kind}`}>
      <div className="artifact-document-corner" aria-hidden="true" />
      <div className="artifact-document-mark">
        <FileIcon size={22} />
        <strong>{label}</strong>
      </div>
      <div className="artifact-document-lines" aria-hidden="true">
        <i /><i /><i /><i />
      </div>
      <div className="artifact-document-name">{artifact.name}</div>
      {artifact.description && <p>{artifact.description}</p>}
    </div>
  );
}

interface MediaTransform {
  scale: number;
  x: number;
  y: number;
}

/** Pointer capture keeps a drag alive when the cursor leaves the frame, but it
 *  THROWS for a pointer the browser no longer considers active — and an
 *  exception mid-handler would abort the pan it was starting. */
function capture(el: Element, pointerId: number): void {
  try {
    (el as HTMLElement).setPointerCapture?.(pointerId);
  } catch {
    /* not capturable; the gesture still works, it just ends at the edge */
  }
}

/** Zoom ceiling (800%) and floor (10%). The floor used to be 1.0 — nothing
 *  could be made SMALLER than the frame, which is half of what a zoom control
 *  is for. */
const MAX_MEDIA_ZOOM = 8;
const MIN_MEDIA_ZOOM = 0.1;
/** Wheel notch -> zoom factor. Multiplicative, so a notch feels the same at 10%
 *  as at 800%; a fixed step crawls when zoomed in and lurches when zoomed out. */
const WHEEL_ZOOM_STEP = 1.15;
/** How far a plain (unmodified) wheel notch pans. */
const WHEEL_PAN_STEP = 80;

function ZoomableMedia({
  kind,
  url,
  name,
}: {
  kind: "image" | "video";
  url: string;
  name: string;
}) {
  const frameRef = useRef<HTMLDivElement | null>(null);
  const transformRef = useRef<HTMLDivElement | null>(null);
  const pointersRef = useRef(new Map<number, { x: number; y: number }>());
  const valueRef = useRef<MediaTransform>({ scale: 1, x: 0, y: 0 });
  const gestureRef = useRef({
    distance: 0,
    scale: 1,
    x: 0,
    y: 0,
    centerX: 0,
    centerY: 0,
    pointerX: 0,
    pointerY: 0,
  });
  const [percent, setPercent] = useState(100);
  const [isFullscreen, setIsFullscreen] = useState(false);

  const apply = (next: MediaTransform) => {
    const frame = frameRef.current;
    const target = transformRef.current;
    if (!frame || !target) return;
    const scale = Math.max(MIN_MEDIA_ZOOM, Math.min(MAX_MEDIA_ZOOM, next.scale));
    // Panning is bounded by how far the scaled content actually overhangs the
    // frame. Under 1 there is no overhang, so it stays centred instead of
    // drifting off into the surrounding black.
    const maxX = Math.max(0, (frame.clientWidth * (scale - 1)) / 2);
    const maxY = Math.max(0, (frame.clientHeight * (scale - 1)) / 2);
    const value = {
      scale,
      x: Math.max(-maxX, Math.min(maxX, next.x)),
      y: Math.max(-maxY, Math.min(maxY, next.y)),
    };
    valueRef.current = value;
    target.style.transform = `translate3d(${value.x}px, ${value.y}px, 0) scale(${value.scale})`;
    frame.classList.toggle("is-zoomed", scale > 1.01);
    const rounded = Math.round(scale * 100);
    setPercent((current) => current === rounded ? current : rounded);
  };

  const reset = () => apply({ scale: 1, x: 0, y: 0 });

  /** Zoom by a factor, keeping whatever sits under the cursor where it is.
   *  Scaling about the frame's centre instead — what this did before — walks
   *  the thing you were looking at straight out of view. */
  const zoomAt = (factor: number, clientX?: number, clientY?: number) => {
    const frame = frameRef.current;
    const current = valueRef.current;
    const scale = Math.max(
      MIN_MEDIA_ZOOM,
      Math.min(MAX_MEDIA_ZOOM, current.scale * factor),
    );
    if (!frame || clientX === undefined || clientY === undefined) {
      apply({ ...current, scale });
      return;
    }
    const rect = frame.getBoundingClientRect();
    // Cursor offset from the frame's centre, which is the transform origin.
    const ox = clientX - rect.left - rect.width / 2;
    const oy = clientY - rect.top - rect.height / 2;
    const ratio = scale / current.scale;
    apply({
      scale,
      x: ox - (ox - current.x) * ratio,
      y: oy - (oy - current.y) * ratio,
    });
  };

  const zoomBy = (factor: number) => zoomAt(factor);

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if ((event.target as Element).closest("button")) return;
    // Every pointer type, not just touch. A mouse could not pan a zoomed image
    // at all, which made zooming past the frame edge pointless.
    pointersRef.current.set(event.pointerId, { x: event.clientX, y: event.clientY });
    const points = [...pointersRef.current.values()];
    const current = valueRef.current;
    if (points.length >= 2) {
      const [a, b] = points;
      gestureRef.current = {
        distance: Math.hypot(b.x - a.x, b.y - a.y),
        scale: current.scale,
        x: current.x,
        y: current.y,
        centerX: (a.x + b.x) / 2,
        centerY: (a.y + b.y) / 2,
        pointerX: 0,
        pointerY: 0,
      };
      for (const id of pointersRef.current.keys()) capture(event.currentTarget, id);
      return;
    }
    // A video's own controls need their clicks, so it only grabs the pointer
    // once there is somewhere to pan to.
    if (kind === "video" && current.scale <= 1) return;
    gestureRef.current = {
      ...gestureRef.current,
      x: current.x,
      y: current.y,
      pointerX: event.clientX,
      pointerY: event.clientY,
    };
    capture(event.currentTarget, event.pointerId);
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!pointersRef.current.has(event.pointerId)) return;
    pointersRef.current.set(event.pointerId, { x: event.clientX, y: event.clientY });
    const points = [...pointersRef.current.values()];
    const gesture = gestureRef.current;
    if (points.length >= 2) {
      event.preventDefault();
      const [a, b] = points;
      const distance = Math.hypot(b.x - a.x, b.y - a.y);
      const centerX = (a.x + b.x) / 2;
      const centerY = (a.y + b.y) / 2;
      apply({
        scale: gesture.scale * (distance / Math.max(1, gesture.distance)),
        x: gesture.x + centerX - gesture.centerX,
        y: gesture.y + centerY - gesture.centerY,
      });
    } else if (points.length === 1) {
      event.preventDefault();
      apply({
        scale: valueRef.current.scale,
        x: gesture.x + event.clientX - gesture.pointerX,
        y: gesture.y + event.clientY - gesture.pointerY,
      });
    }
  };

  const onPointerEnd = (event: ReactPointerEvent<HTMLDivElement>) => {
    pointersRef.current.delete(event.pointerId);
    const remaining = [...pointersRef.current.values()];
    if (remaining.length === 1) {
      const current = valueRef.current;
      gestureRef.current = {
        ...gestureRef.current,
        x: current.x,
        y: current.y,
        pointerX: remaining[0].x,
        pointerY: remaining[0].y,
      };
    }
  };

  /** Plain wheel pans (shift for sideways); ctrl/cmd zooms at the cursor. A
   *  bare wheel used to return early and do nothing whatsoever, which is what
   *  "you can't scroll" meant. */
  const onWheel = (event: ReactWheelEvent<HTMLDivElement>) => {
    event.preventDefault();
    if (event.ctrlKey || event.metaKey) {
      zoomAt(
        event.deltaY < 0 ? WHEEL_ZOOM_STEP : 1 / WHEEL_ZOOM_STEP,
        event.clientX,
        event.clientY,
      );
      return;
    }
    const current = valueRef.current;
    const step = (delta: number) =>
      delta === 0 ? 0 : delta > 0 ? -WHEEL_PAN_STEP : WHEEL_PAN_STEP;
    const sideways = event.shiftKey;
    apply({
      scale: current.scale,
      x: current.x + step(sideways ? event.deltaY || event.deltaX : event.deltaX),
      y: current.y + (sideways ? 0 : step(event.deltaY)),
    });
  };

  const toggleFullscreen = () => {
    const frame = frameRef.current;
    if (!frame) return;
    if (document.fullscreenElement === frame) void document.exitFullscreen();
    else void frame.requestFullscreen().catch(() => undefined);
  };

  // Keep the button honest when fullscreen is left by Escape or the OS chrome
  // rather than by the button.
  useEffect(() => {
    const sync = () => setIsFullscreen(document.fullscreenElement === frameRef.current);
    document.addEventListener("fullscreenchange", sync);
    return () => document.removeEventListener("fullscreenchange", sync);
  }, []);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const current = valueRef.current;
    const pan = event.shiftKey ? 200 : 60;
    const nudge = (dx: number, dy: number) => {
      event.preventDefault();
      apply({ scale: current.scale, x: current.x + dx, y: current.y + dy });
    };
    switch (event.key) {
      case "+":
      case "=":
        event.preventDefault();
        zoomBy(WHEEL_ZOOM_STEP);
        break;
      case "-":
      case "_":
        event.preventDefault();
        zoomBy(1 / WHEEL_ZOOM_STEP);
        break;
      case "0":
        event.preventDefault();
        reset();
        break;
      case "f":
      case "F":
        event.preventDefault();
        toggleFullscreen();
        break;
      case "ArrowLeft": nudge(pan, 0); break;
      case "ArrowRight": nudge(-pan, 0); break;
      case "ArrowUp": nudge(0, pan); break;
      case "ArrowDown": nudge(0, -pan); break;
      default:
        break;
    }
  };

  return (
    <div
      ref={frameRef}
      className={`artifact-zoom-stage kind-${kind}`}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerEnd}
      onPointerCancel={onPointerEnd}
      onWheel={onWheel}
      onKeyDown={onKeyDown}
      onDoubleClick={(event) =>
        percent > 100 ? reset() : zoomAt(2, event.clientX, event.clientY)
      }
    >
      <div ref={transformRef} className="artifact-media-transform">
        {kind === "image" ? (
          <img className="artifact-preview-image" src={url} alt={name} draggable={false} />
        ) : (
          <div className="artifact-preview-video-wrap">
            <video
              className="artifact-preview-video"
              src={url}
              controls
              playsInline
              preload="metadata"
              aria-label={`Video: ${name}`}
            />
          </div>
        )}
      </div>
      <span className="artifact-pinch-hint" aria-hidden="true">Pinch to zoom</span>
      <div className="artifact-media-zoom" aria-label="Media zoom controls">
        <button
          type="button"
          onClick={() => zoomBy(1 / WHEEL_ZOOM_STEP)}
          disabled={percent <= MIN_MEDIA_ZOOM * 100}
          aria-label="Zoom out"
          title="Zoom out (−)"
        >
          −
        </button>
        <button
          type="button"
          onClick={reset}
          disabled={percent === 100}
          aria-label="Fit to window"
          title="Fit to window (0)"
        >
          {percent}%
        </button>
        <button
          type="button"
          onClick={() => zoomBy(WHEEL_ZOOM_STEP)}
          disabled={percent >= MAX_MEDIA_ZOOM * 100}
          aria-label="Zoom in"
          title="Zoom in (+)"
        >
          +
        </button>
        <button
          type="button"
          className="artifact-media-full"
          onClick={toggleFullscreen}
          aria-pressed={isFullscreen}
          aria-label={isFullscreen ? "Exit full screen" : "Full screen"}
          title={isFullscreen ? "Exit full screen (F)" : "Full screen (F)"}
        >
          {isFullscreen ? "⤡" : "⤢"}
        </button>
      </div>
    </div>
  );
}

/** The actual artifact surface shared by chat previews, the chat lightbox, and
 * the Artifacts pane. Office formats cannot be rendered faithfully by a webview
 * without uploading them to a third party, so they get a private, local
 * first-page treatment rather than a misleading broken iframe. */
export function ArtifactPreview({
  artifact,
  url,
  mode,
}: {
  artifact: ArtifactPreviewRecord;
  url: string | null;
  mode: "inline" | "full";
}) {
  const kind = artifactKind(artifact);
  const [text, setText] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const needsText = kind === "markdown" || (kind === "html" && mode === "inline") || kind === "text";

  useEffect(() => {
    if (!url || !needsText) return;
    let cancelled = false;
    setText(null);
    setError(null);
    fetch(url)
      .then((response) => response.ok ? response.text() : Promise.reject(new Error(`HTTP ${response.status}`)))
      .then((contents) => !cancelled && setText(contents))
      .catch((reason) => !cancelled && setError(String(reason?.message ?? reason)));
    return () => { cancelled = true; };
  }, [url, needsText]);

  if (!url) return <div className="artifact-preview-status">Preview unavailable while disconnected.</div>;

  if (kind === "image") {
    return mode === "full"
      ? <ZoomableMedia kind="image" url={url} name={artifact.name} />
      : <img className="artifact-preview-image" src={url} alt={artifact.name} loading="lazy" />;
  }

  if (kind === "video") {
    if (mode === "full") return <ZoomableMedia kind="video" url={url} name={artifact.name} />;
    return (
      <div className="artifact-preview-video-wrap">
        <video
          className="artifact-preview-video"
          src={url}
          controls
          playsInline
          preload="metadata"
          aria-label={`Video: ${artifact.name}`}
        />
        <span className="artifact-video-hint" aria-hidden="true"><PlayIcon size={13} /> video</span>
      </div>
    );
  }

  if (kind === "pdf") {
    return (
      <Suspense fallback={<div className="artifact-preview-status">Opening PDF…</div>}>
        {mode === "full" ? <PdfViewer src={url} title={artifact.name} /> : <PdfPreview src={url} title={artifact.name} />}
      </Suspense>
    );
  }

  if (kind === "html" && mode === "full") {
    return <HtmlPreview url={url} title={`Preview of ${artifact.name}`} className="artifact-preview-html" />;
  }

  if (needsText) {
    if (error) return <div className="artifact-preview-status is-error">{error}</div>;
    if (text == null) return <div className="artifact-preview-status">Loading preview…</div>;
    if (kind === "markdown") {
      return <div className={`artifact-preview-markdown mode-${mode}`}><Markdown text={text} /></div>;
    }
    return <pre className={`artifact-preview-text mode-${mode}`}>{text}</pre>;
  }

  return <DocumentPlaceholder artifact={artifact} kind={kind} />;
}
