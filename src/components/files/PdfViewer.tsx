import { useEffect, useRef, useState } from "react";
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

/**
 * PDF.js canvas viewer. Tauri's Linux WebKit runtime has no dependable native
 * PDF plug-in, so an iframe can succeed at loading while displaying a blank
 * page. Rendering here keeps the viewer identical on desktop and LAN clients.
 */
export function PdfViewer({ src, title }: { src: string; title: string }) {
  const stageRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const renderQueueRef = useRef<Promise<void>>(Promise.resolve());
  const renderGenerationRef = useRef(0);

  const [document, setDocument] = useState<PDFDocumentProxy | null>(null);
  const [pageNumber, setPageNumber] = useState(1);
  const [stageWidth, setStageWidth] = useState(0);
  const [zoom, setZoom] = useState(1);
  const [loading, setLoading] = useState(true);
  const [rendering, setRendering] = useState(false);
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

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!document || !canvas || stageWidth <= 0) return;

    let disposed = false;
    let renderTask: RenderTask | null = null;
    const generation = ++renderGenerationRef.current;
    setRendering(true);
    setError(null);

    const render = async () => {
      if (disposed) return;
      const page = await document.getPage(pageNumber);
      if (disposed) return;

      const unscaled = page.getViewport({ scale: 1 });
      const availableWidth = Math.max(180, stageWidth - 40);
      const fitScale = availableWidth / unscaled.width;
      const viewport = page.getViewport({ scale: fitScale * zoom });
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

    const queued = renderQueueRef.current.catch(() => undefined).then(render);
    renderQueueRef.current = queued;
    void queued
      .catch((renderError) => {
        if (!disposed && !(renderError instanceof RenderingCancelledException)) {
          setError(errorMessage(renderError));
        }
      })
      .finally(() => {
        if (!disposed && renderGenerationRef.current === generation) setRendering(false);
      });

    return () => {
      disposed = true;
      renderTask?.cancel();
    };
  }, [document, pageNumber, stageWidth, zoom]);

  const pageCount = document?.numPages ?? 0;
  const goToPage = (next: number) => {
    if (!pageCount) return;
    setPageNumber(Math.max(1, Math.min(pageCount, next)));
    stageRef.current?.scrollTo({ top: 0, left: 0 });
  };
  const changeZoom = (delta: number) => {
    setZoom((current) => Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, current + delta)));
  };

  return (
    <div className="files-pdf-viewer" aria-label={`PDF viewer: ${title}`}>
      <div className="files-pdf-toolbar">
        <div className="files-pdf-controls" aria-label="Page navigation">
          <button
            type="button"
            className="files-pdf-tool"
            aria-label="Previous page"
            title="Previous page"
            disabled={!document || pageNumber <= 1}
            onClick={() => goToPage(pageNumber - 1)}
          >
            ‹
          </button>
          <span className="files-pdf-page" aria-live="polite">
            {pageCount ? `${pageNumber} / ${pageCount}` : "— / —"}
          </span>
          <button
            type="button"
            className="files-pdf-tool"
            aria-label="Next page"
            title="Next page"
            disabled={!document || pageNumber >= pageCount}
            onClick={() => goToPage(pageNumber + 1)}
          >
            ›
          </button>
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

      <div ref={stageRef} className="files-pdf-stage">
        {(loading || rendering) && !error && (
          <div className="files-pdf-status">{loading ? "Opening PDF…" : "Rendering…"}</div>
        )}
        {error && <div className="files-pdf-status files-error">{error}</div>}
        <canvas ref={canvasRef} className={rendering ? "is-rendering" : ""} />
      </div>
    </div>
  );
}
