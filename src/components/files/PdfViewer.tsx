import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import {
  getDocument,
  GlobalWorkerOptions,
  RenderingCancelledException,
  type PDFDocumentProxy,
  type RenderTask,
} from "pdfjs-dist/legacy/build/pdf.mjs";
import pdfWorkerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";

GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

const MIN_ZOOM = 0.5;
const MAX_ZOOM = 2.5;
const ZOOM_STEP = 0.25;

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return "The PDF could not be opened.";
}

function ContinuousPdfPage({
  document,
  pageNumber,
  stageWidth,
  zoom,
  scrollRoot,
}: {
  document: PDFDocumentProxy;
  pageNumber: number;
  stageWidth: number;
  zoom: number;
  scrollRoot: RefObject<HTMLDivElement>;
}) {
  const shellRef = useRef<HTMLElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [active, setActive] = useState(pageNumber <= 2);
  const [aspectRatio, setAspectRatio] = useState(8.5 / 11);
  const [rendering, setRendering] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Render shortly before a page reaches the viewport, then keep the bitmap.
  // This makes a 200-page document scroll like one document without allocating
  // 200 high-DPI canvases on open.
  useEffect(() => {
    const shell = shellRef.current;
    const root = scrollRoot.current;
    if (!shell || !root || active) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setActive(true);
          observer.disconnect();
        }
      },
      { root, rootMargin: "900px 0px", threshold: 0.01 },
    );
    observer.observe(shell);
    return () => observer.disconnect();
  }, [active, scrollRoot]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!active || !canvas || stageWidth <= 0) return;
    let disposed = false;
    let renderTask: RenderTask | null = null;
    setRendering(true);
    setError(null);

    const render = async () => {
      const page = await document.getPage(pageNumber);
      if (disposed) return;
      const unscaled = page.getViewport({ scale: 1 });
      setAspectRatio(unscaled.width / unscaled.height);
      const availableWidth = Math.max(180, stageWidth - 40);
      const viewport = page.getViewport({ scale: (availableWidth / unscaled.width) * zoom });
      const outputScale = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.floor(viewport.width * outputScale);
      canvas.height = Math.floor(viewport.height * outputScale);
      canvas.style.width = `${Math.floor(viewport.width)}px`;
      canvas.style.height = `${Math.floor(viewport.height)}px`;
      renderTask = page.render({
        canvas,
        viewport,
        transform: outputScale === 1 ? undefined : [outputScale, 0, 0, outputScale, 0, 0],
      });
      await renderTask.promise;
    };

    void render()
      .catch((renderError) => {
        if (!disposed && !(renderError instanceof RenderingCancelledException)) {
          setError(errorMessage(renderError));
        }
      })
      .finally(() => !disposed && setRendering(false));

    return () => {
      disposed = true;
      renderTask?.cancel();
    };
  }, [active, document, pageNumber, stageWidth, zoom]);

  const estimatedWidth = Math.max(180, stageWidth - 40) * zoom;
  return (
    <section
      ref={shellRef}
      className="files-pdf-page-shell"
      data-pdf-page={pageNumber}
      aria-label={`Page ${pageNumber}`}
      style={{ width: estimatedWidth, minHeight: estimatedWidth / aspectRatio }}
    >
      {(rendering || !active) && !error && (
        <span className="files-pdf-page-status">{active ? `Rendering page ${pageNumber}…` : `Page ${pageNumber}`}</span>
      )}
      {error && <span className="files-pdf-page-status files-error">Page {pageNumber} unavailable</span>}
      <canvas ref={canvasRef} className={rendering ? "is-rendering" : ""} />
      <span className="files-pdf-page-number" aria-hidden="true">{pageNumber}</span>
    </section>
  );
}

/**
 * PDF.js canvas viewer. Tauri's Linux WebKit runtime has no dependable native
 * PDF plug-in, so an iframe can succeed at loading while displaying a blank
 * page. Rendering here keeps the viewer identical on desktop and LAN clients.
 */
export function PdfViewer({ src, title }: { src: string; title: string }) {
  const stageRef = useRef<HTMLDivElement | null>(null);

  const [document, setDocument] = useState<PDFDocumentProxy | null>(null);
  const [pageNumber, setPageNumber] = useState(1);
  const [stageWidth, setStageWidth] = useState(0);
  const [zoom, setZoom] = useState(1);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;

    const updateWidth = () => setStageWidth(stage.clientWidth);
    updateWidth();
    const observer = new ResizeObserver(updateWidth);
    observer.observe(stage);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let disposed = false;
    const task = getDocument({ url: src });

    setDocument(null);
    setPageNumber(1);
    setZoom(1);
    setLoading(true);
    setError(null);

    void task.promise
      .then((pdf) => {
        if (disposed) return;
        setDocument(pdf);
      })
      .catch((loadError) => {
        if (!disposed) setError(errorMessage(loadError));
      })
      .finally(() => {
        if (!disposed) setLoading(false);
      });

    return () => {
      disposed = true;
      void task.destroy();
    };
  }, [src]);

  const pageCount = document?.numPages ?? 0;
  const changeZoom = (delta: number) => {
    setZoom((current) => Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, current + delta)));
  };
  const updateCurrentPage = useCallback(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const center = stage.scrollTop + stage.clientHeight * 0.42;
    let nearest = 1;
    let distance = Number.POSITIVE_INFINITY;
    stage.querySelectorAll<HTMLElement>("[data-pdf-page]").forEach((page) => {
      const pageCenter = page.offsetTop + page.offsetHeight / 2;
      const nextDistance = Math.abs(center - pageCenter);
      if (nextDistance < distance) {
        distance = nextDistance;
        nearest = Number(page.dataset.pdfPage) || 1;
      }
    });
    setPageNumber((current) => current === nearest ? current : nearest);
  }, []);

  return (
    <div className="files-pdf-viewer" aria-label={`PDF viewer: ${title}`}>
      <div className="files-pdf-toolbar">
        <div className="files-pdf-controls" aria-label="Current page">
          <span className="files-pdf-page" aria-live="polite">
            {pageCount ? `Page ${pageNumber} / ${pageCount}` : "— / —"}
          </span>
        </div>

        <div className="files-pdf-controls" aria-label="Zoom controls">
          <button
            type="button"
            className="files-pdf-tool"
            aria-label="Zoom out"
            title="Zoom out"
            disabled={!document || zoom <= MIN_ZOOM}
            onClick={() => changeZoom(-ZOOM_STEP)}
          >
            −
          </button>
          <button
            type="button"
            className="files-pdf-zoom"
            title="Reset to fit width"
            disabled={!document}
            onClick={() => setZoom(1)}
          >
            Fit · {Math.round(zoom * 100)}%
          </button>
          <button
            type="button"
            className="files-pdf-tool"
            aria-label="Zoom in"
            title="Zoom in"
            disabled={!document || zoom >= MAX_ZOOM}
            onClick={() => changeZoom(ZOOM_STEP)}
          >
            +
          </button>
        </div>
      </div>

      <div ref={stageRef} className="files-pdf-stage" onScroll={updateCurrentPage}>
        {loading && !error && <div className="files-pdf-status">Opening PDF…</div>}
        {error && <div className="files-pdf-status files-error">{error}</div>}
        {document && (
          <div className="files-pdf-pages">
            {Array.from({ length: document.numPages }, (_, index) => (
              <ContinuousPdfPage
                key={index + 1}
                document={document}
                pageNumber={index + 1}
                stageWidth={stageWidth}
                zoom={zoom}
                scrollRoot={stageRef}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** Lightweight first-page renderer for an artifact sitting in the chat log.
 * It deliberately has no toolbar: tapping the preview opens the full viewer. */
export function PdfPreview({ src, title }: { src: string; title: string }) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const stage = stageRef.current;
    const canvas = canvasRef.current;
    if (!stage || !canvas) return;

    let disposed = false;
    let renderTask: RenderTask | null = null;
    const task = getDocument({ url: src });

    const render = async () => {
      const pdf = await task.promise;
      if (disposed) return;
      const page = await pdf.getPage(1);
      if (disposed) return;
      const base = page.getViewport({ scale: 1 });
      const width = Math.max(220, stage.clientWidth);
      const viewport = page.getViewport({ scale: width / base.width });
      const outputScale = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.floor(viewport.width * outputScale);
      canvas.height = Math.floor(viewport.height * outputScale);
      canvas.style.width = `${Math.floor(viewport.width)}px`;
      canvas.style.height = `${Math.floor(viewport.height)}px`;
      renderTask = page.render({
        canvas,
        viewport,
        transform: outputScale === 1 ? undefined : [outputScale, 0, 0, outputScale, 0, 0],
      });
      await renderTask.promise;
    };

    setLoading(true);
    setError(null);
    void render()
      .catch((reason) => {
        if (!disposed && !(reason instanceof RenderingCancelledException)) setError(errorMessage(reason));
      })
      .finally(() => !disposed && setLoading(false));

    return () => {
      disposed = true;
      renderTask?.cancel();
      void task.destroy();
    };
  }, [src]);

  return (
    <div ref={stageRef} className="artifact-pdf-preview" aria-label={`First page of ${title}`}>
      {loading && !error && <div className="artifact-preview-status">Rendering first page…</div>}
      {error && <div className="artifact-preview-status is-error">PDF preview unavailable</div>}
      <canvas ref={canvasRef} />
    </div>
  );
}
