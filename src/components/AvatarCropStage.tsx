import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { CropSpec } from "../lib/sidebarImage";

/** Workspace art may use the server's full 512 KB allowance. Machine/Hermes
 *  avatars pass a tighter 64 KB cap in their CropSpec. */
const DEFAULT_MAX_IMAGE_BYTES = 512 * 1024;
/** Max size of a file the user may hand us before re-encoding. */
export const MAX_UPLOAD_BYTES = 10 * 1024 * 1024;

/** Zoom is relative to "image exactly covers the circle" (1 = cover fit). */
const CROP_ZOOM_MIN = 1;
const CROP_ZOOM_MAX = 5;
const CROP_ZOOM_STEP = 1.25;
const WHEEL_SENSITIVITY = 0.0015;
/** Gap between the circle and the canvas edge, CSS px. */
const CIRCLE_INSET = 16;
/** Debounce for the live-preview callback while dragging/zooming. */
const PREVIEW_DEBOUNCE_MS = 120;

/** Approximate decoded byte size of a base64 data URL's payload. */
function dataUrlBytes(url: string): number {
  const comma = url.indexOf(",");
  return Math.floor(((url.length - comma - 1) * 3) / 4);
}

function ZoomGlyph({ plus }: { plus?: boolean }) {
  return (
    <svg
      width={15}
      height={15}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <circle cx="11" cy="11" r="6.5" />
      <path d="M20 20l-4.2-4.2" />
      <path d="M8 11h6" />
      {plus && <path d="M11 8v6" />}
    </svg>
  );
}

export interface AvatarCropStageHandle {
  /** Render the circle's content to `spec.size` on `spec.background` and
   *  return the data URL. Throws when the result stays over the size cap
   *  even after a lower-quality retry, or when the image never loaded. */
  save(): string;
  reset(): void;
}

/**
 * The interactive crop surface: canvas with a circular mask over the chosen
 * image, drag to reposition, wheel/slider/[-][+] to zoom, reset. One shared
 * implementation used by the standalone AvatarCropModal AND the embedded
 * stage inside the Customize Profile popup; the save math lives here once.
 */
export const AvatarCropStage = forwardRef<
  AvatarCropStageHandle,
  {
    src: string;
    spec: CropSpec;
    onReadyChange?: (ready: boolean) => void;
    onError?: (message: string) => void;
    /** Debounced data-URL of the current circle, for live previews. */
    onPreview?: (dataUrl: string) => void;
  }
>(function AvatarCropStage({ src, spec, onReadyChange, onError, onPreview }, ref) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);
  /** Canvas CSS size + mask-circle radius, measured once on mount. */
  const viewRef = useRef({ size: 0, radius: 0 });
  /** Image-center offset from the circle center, CSS px. */
  const panRef = useRef({ x: 0, y: 0 });
  const zoomRef = useRef(1);
  const frameRef = useRef<number | null>(null);
  const previewTimerRef = useRef<number | null>(null);
  const dragRef = useRef<{ id: number; x: number; y: number } | null>(null);
  const [zoom, setZoom] = useState(1);
  const [ready, setReady] = useState(false);

  const onReadyChangeRef = useRef(onReadyChange);
  onReadyChangeRef.current = onReadyChange;
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const onPreviewRef = useRef(onPreview);
  onPreviewRef.current = onPreview;

  /** Scale at which the image exactly covers the circle (zoom = 1). */
  const coverScale = useCallback(() => {
    const img = imgRef.current;
    const { radius } = viewRef.current;
    if (!img || !radius) return 1;
    return (radius * 2) / Math.min(img.naturalWidth, img.naturalHeight);
  }, []);

  /** Keep the image covering the whole circle: never let a gap show. */
  const clampPan = useCallback(() => {
    const img = imgRef.current;
    if (!img) return;
    const { radius } = viewRef.current;
    const scale = coverScale() * zoomRef.current;
    const maxX = Math.max(0, (img.naturalWidth * scale) / 2 - radius);
    const maxY = Math.max(0, (img.naturalHeight * scale) / 2 - radius);
    panRef.current = {
      x: Math.max(-maxX, Math.min(maxX, panRef.current.x)),
      y: Math.max(-maxY, Math.min(maxY, panRef.current.y)),
    };
  }, [coverScale]);

  /** Render the circle's content onto a fresh square canvas at `size` px. */
  const renderOutput = useCallback(
    (size: number, quality: number, mime = spec.mime): string | null => {
      const img = imgRef.current;
      const { radius } = viewRef.current;
      if (!img || !radius) return null;
      const out = document.createElement("canvas");
      out.width = size;
      out.height = size;
      const ctx = out.getContext("2d");
      if (!ctx) return null;
      // Map the mask circle's bounding square onto the output square.
      const k = size / (radius * 2);
      const scale = coverScale() * zoomRef.current * k;
      const w = img.naturalWidth * scale;
      const h = img.naturalHeight * scale;
      ctx.fillStyle = spec.background;
      ctx.fillRect(0, 0, size, size);
      ctx.imageSmoothingQuality = "high";
      ctx.drawImage(
        img,
        size / 2 + panRef.current.x * k - w / 2,
        size / 2 + panRef.current.y * k - h / 2,
        w,
        h,
      );
      return out.toDataURL(mime, quality);
    },
    [coverScale, spec.background, spec.mime],
  );

  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    const img = imgRef.current;
    const { size, radius } = viewRef.current;
    if (!canvas || !img || !size) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, size, size);
    ctx.fillStyle = spec.background;
    ctx.fillRect(0, 0, size, size);
    const scale = coverScale() * zoomRef.current;
    const w = img.naturalWidth * scale;
    const h = img.naturalHeight * scale;
    const cx = size / 2;
    const cy = size / 2;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(img, cx + panRef.current.x - w / 2, cy + panRef.current.y - h / 2, w, h);
    // Dim everything outside the circle (even-odd punches the hole).
    ctx.beginPath();
    ctx.rect(0, 0, size, size);
    ctx.arc(cx, cy, radius, 0, Math.PI * 2);
    ctx.fillStyle = "rgba(5, 6, 10, 0.62)";
    ctx.fill("evenodd");
    // Crisp circle stroke.
    ctx.beginPath();
    ctx.arc(cx, cy, radius, 0, Math.PI * 2);
    ctx.strokeStyle = "rgba(217, 163, 92, 0.8)";
    ctx.lineWidth = 1.5;
    ctx.stroke();
  }, [coverScale, spec.background]);

  /** rAF-throttled redraw: at most one paint per frame during drag/zoom.
   *  Also debounces the (optional) live-preview snapshot. */
  const requestDraw = useCallback(() => {
    if (onPreviewRef.current) {
      if (previewTimerRef.current !== null) window.clearTimeout(previewTimerRef.current);
      previewTimerRef.current = window.setTimeout(() => {
        previewTimerRef.current = null;
        const url = renderOutput(spec.size, spec.quality);
        if (url) onPreviewRef.current?.(url);
      }, PREVIEW_DEBOUNCE_MS);
    }
    if (frameRef.current !== null) return;
    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = null;
      draw();
    });
  }, [draw, renderOutput, spec.size, spec.quality]);

  const applyZoom = useCallback(
    (next: number) => {
      const clamped = Math.min(CROP_ZOOM_MAX, Math.max(CROP_ZOOM_MIN, next));
      const factor = clamped / zoomRef.current;
      if (factor !== 1) {
        zoomRef.current = clamped;
        // Zoom toward the circle's center: the point under it stays put.
        panRef.current = {
          x: panRef.current.x * factor,
          y: panRef.current.y * factor,
        };
        clampPan();
        setZoom(clamped);
        requestDraw();
      }
    },
    [clampPan, requestDraw],
  );
  const applyZoomRef = useRef(applyZoom);
  applyZoomRef.current = applyZoom;

  const reset = useCallback(() => {
    zoomRef.current = 1;
    panRef.current = { x: 0, y: 0 };
    setZoom(1);
    requestDraw();
  }, [requestDraw]);

  useImperativeHandle(
    ref,
    () => ({
      reset,
      save() {
        if (!imgRef.current) throw new Error("The image has not finished loading.");
        const maxBytes = spec.maxBytes ?? DEFAULT_MAX_IMAGE_BYTES;
        // Some WebKit builds silently encode WebP requests as PNG, where the
        // quality argument has no effect. Try progressively smaller/leaner
        // outputs, then use JPEG as the reliable opaque fallback.
        const sizes = [
          spec.size,
          Math.round(spec.size * 0.875),
          Math.round(spec.size * 0.75),
        ];
        const attempts = [
          ...sizes.flatMap((size) => [
            { size, mime: spec.mime, quality: spec.quality },
            { size, mime: spec.mime, quality: Math.min(spec.quality, 0.68) },
          ]),
          ...sizes.flatMap((size) => [
            { size, mime: "image/jpeg", quality: Math.min(spec.quality, 0.78) },
            { size, mime: "image/jpeg", quality: 0.58 },
          ]),
        ];
        for (const attempt of attempts) {
          const url = renderOutput(attempt.size, attempt.quality, attempt.mime);
          if (url && dataUrlBytes(url) <= maxBytes) return url;
        }
        throw new Error(
          "That image could not be compressed enough. Try a different one.",
        );
      },
    }),
    [renderOutput, reset, spec.size, spec.quality, spec.maxBytes],
  );

  // Size the canvas once from its laid-out width (dpr-scaled backing store).
  useLayoutEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const size = canvas.clientWidth || 320;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(size * dpr);
    canvas.height = Math.round(size * dpr);
    viewRef.current = { size, radius: size / 2 - CIRCLE_INSET };
  }, []);

  // Load the source image, then land on the centered cover fit.
  useEffect(() => {
    const img = new Image();
    img.onload = () => {
      imgRef.current = img;
      setReady(true);
      onReadyChangeRef.current?.(true);
      reset();
    };
    img.onerror = () => onErrorRef.current?.("That file is not a readable image.");
    img.src = src;
    return () => {
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
      if (previewTimerRef.current !== null) window.clearTimeout(previewTimerRef.current);
    };
  }, [src, reset]);

  // Wheel zoom needs a non-passive native listener so preventDefault works;
  // stopPropagation too, since ctrl+wheel is globally bound to interface zoom.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      e.stopPropagation();
      applyZoomRef.current(zoomRef.current * Math.exp(-e.deltaY * WHEEL_SENSITIVITY));
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, []);

  return (
    <div className="crop-stage-wrap">
      <div className="crop-stage">
        <canvas
          ref={canvasRef}
          className="crop-canvas"
          onPointerDown={(e) => {
            e.preventDefault();
            e.currentTarget.setPointerCapture(e.pointerId);
            dragRef.current = { id: e.pointerId, x: e.clientX, y: e.clientY };
          }}
          onPointerMove={(e) => {
            const drag = dragRef.current;
            if (!drag || drag.id !== e.pointerId) return;
            panRef.current = {
              x: panRef.current.x + (e.clientX - drag.x),
              y: panRef.current.y + (e.clientY - drag.y),
            };
            drag.x = e.clientX;
            drag.y = e.clientY;
            clampPan();
            requestDraw();
          }}
          onPointerUp={() => (dragRef.current = null)}
          onPointerCancel={() => (dragRef.current = null)}
        />
      </div>

      <div className="crop-zoom-row">
        <button
          type="button"
          className="icon-btn"
          aria-label="Zoom out"
          disabled={!ready || zoom <= CROP_ZOOM_MIN}
          onClick={() => applyZoom(zoomRef.current / CROP_ZOOM_STEP)}
        >
          <ZoomGlyph />
        </button>
        <input
          type="range"
          className="crop-zoom-slider"
          min={CROP_ZOOM_MIN}
          max={CROP_ZOOM_MAX}
          step={0.01}
          value={zoom}
          disabled={!ready}
          aria-label="Zoom"
          onChange={(e) => applyZoom(Number(e.target.value))}
        />
        <button
          type="button"
          className="icon-btn"
          aria-label="Zoom in"
          disabled={!ready || zoom >= CROP_ZOOM_MAX}
          onClick={() => applyZoom(zoomRef.current * CROP_ZOOM_STEP)}
        >
          <ZoomGlyph plus />
        </button>
        <button type="button" className="settings-toggle" disabled={!ready} onClick={reset}>
          reset
        </button>
      </div>
      <div className="crop-hint">Drag to reposition. Scroll or use the slider to zoom.</div>
    </div>
  );
});

/**
 * "click here to upload" panel: whole surface is clickable (opens the file
 * dialog) and accepts a dragged-in image file. Validates type + size before
 * handing the File up.
 */
export function AvatarDropzone({
  onFile,
  onError,
}: {
  onFile: (file: File) => void;
  onError: (message: string) => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);

  function accept(file: File | undefined | null) {
    if (!file) return;
    if (!file.type.startsWith("image/")) {
      onError("That file is not a readable image.");
      return;
    }
    if (file.size > MAX_UPLOAD_BYTES) {
      onError("Choose an image smaller than 10 MB.");
      return;
    }
    onFile(file);
  }

  return (
    <div
      className={`avatar-dropzone${dragging ? " drag" : ""}`}
      role="button"
      tabIndex={0}
      aria-label="Upload an image"
      onClick={() => inputRef.current?.click()}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          inputRef.current?.click();
        }
      }}
      onDragOver={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => {
        e.preventDefault();
        setDragging(false);
        accept(e.dataTransfer.files?.[0]);
      }}
    >
      <input
        ref={inputRef}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif"
        style={{ display: "none" }}
        onChange={(e) => {
          accept(e.currentTarget.files?.[0]);
          e.currentTarget.value = "";
        }}
      />
      <div className="avatar-dropzone-title">click here to upload</div>
      <div className="avatar-dropzone-sub">or drop an image file · png, jpeg, webp, gif</div>
    </div>
  );
}
