// Palette extraction + image downscaling for user-crafted themes. Given a
// wallpaper the user picked, extractPalette suggests a full CustomTheme palette
// (base preset + accent + the 10 neutral vars) and returns the dominant
// swatches for a chooser. downscaleImage / makeThemeThumbnail keep imported
// wallpapers at a sane size before they are stored as data URLs.
//
// No dependencies: quantization is a hand-rolled median cut over a downscaled
// canvas readback. The hex/HSL helpers are reused from appearance.ts rather
// than duplicated, so the accent math and this file stay in lockstep.

import {
  NEUTRAL_VAR_KEYS,
  clamp,
  hslToRgb,
  parseHex,
  rgbToHsl,
  toHex,
  type NeutralVarKey,
  type Rgb,
} from "./appearance";

/** The suggested starting point for a custom theme built from an image. base is
 *  a THEMES preset id ("dark" | "light" for now); accent is a "#rrggbb"; colors
 *  covers every NEUTRAL_VAR_KEYS entry (no leading "--"). Feed straight into a
 *  CustomTheme record. */
export interface PaletteSuggestion {
  base: string;
  accent: string;
  colors: Record<NeutralVarKey, string>;
}

/** Result of extractPalette: dominant swatches (6-10 hexes, most-covering
 *  first) plus a ready-to-edit theme suggestion. */
export interface ExtractedPalette {
  swatches: string[];
  suggestion: PaletteSuggestion;
}

// ---- canvas helpers --------------------------------------------------------

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    // Data URLs are same-origin; crossOrigin keeps object URLs untainted too.
    img.crossOrigin = "anonymous";
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("themeCraft: image failed to load"));
    img.src = src;
  });
}

/** Draw any image source onto a fresh cw x ch canvas. Returns the canvas; the
 *  caller reads pixels or exports it. */
function drawToCanvas(source: CanvasImageSource, cw: number, ch: number): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = cw;
  canvas.height = ch;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("themeCraft: 2d canvas context unavailable");
  ctx.drawImage(source, 0, 0, cw, ch);
  return canvas;
}

/** The target longest edge for `source`, never upscaled. */
function fitEdge(w: number, h: number, maxEdge: number): { cw: number; ch: number } {
  const scale = Math.min(1, maxEdge / Math.max(1, w, h));
  return { cw: Math.max(1, Math.round(w * scale)), ch: Math.max(1, Math.round(h * scale)) };
}

/** Draw an image onto a canvas scaled so its longest edge is maxEdge (never
 *  upscaled). Returns the canvas; caller reads pixels or exports it. */
function drawScaled(img: HTMLImageElement, maxEdge: number): HTMLCanvasElement {
  const { cw, ch } = fitEdge(img.naturalWidth || img.width, img.naturalHeight || img.height, maxEdge);
  return drawToCanvas(img, cw, ch);
}

/** Hard ceiling on a picked wallpaper's raw bytes. A larger source can exhaust
 *  the webview just decoding it, so it is rejected before any decode attempt. */
const MAX_SOURCE_BYTES = 30 * 1024 * 1024;

/** Decode `file` onto a canvas whose longest edge is at most maxEdge. Prefers
 *  createImageBitmap with resize hints (the decoder scales during the decode,
 *  so the full-resolution raster need not persist) and falls back to the
 *  HTMLImageElement path where that API is unavailable or refuses the blob. */
async function decodeScaled(file: Blob, maxEdge: number): Promise<HTMLCanvasElement> {
  if (typeof createImageBitmap === "function") {
    let probe: ImageBitmap | null = null;
    try {
      probe = await createImageBitmap(file);
      const { cw, ch } = fitEdge(probe.width, probe.height, maxEdge);
      if (cw >= probe.width) {
        // Already within budget: draw the decoded bitmap as-is.
        try {
          return drawToCanvas(probe, cw, ch);
        } finally {
          probe.close();
          probe = null;
        }
      }
      // Re-decode straight to the capped size so the browser downscales in the
      // decoder rather than after materializing the full-resolution image.
      probe.close();
      probe = null;
      const bitmap = await createImageBitmap(file, {
        resizeWidth: cw,
        resizeHeight: ch,
        resizeQuality: "high",
      });
      try {
        return drawToCanvas(bitmap, cw, ch);
      } finally {
        bitmap.close();
      }
    } catch {
      // Some webviews reject createImageBitmap for particular blobs; the
      // HTMLImageElement path below still handles them.
      probe?.close();
    }
  }
  const url = URL.createObjectURL(file);
  try {
    return drawScaled(await loadImage(url), maxEdge);
  } finally {
    URL.revokeObjectURL(url);
  }
}

/**
 * Downscale an image Blob to a JPEG data URL whose longest edge is at most
 * maxEdge, at the given quality (0..1). Used by the UI to store a picked
 * wallpaper at a reasonable size (~1600px edge, ~0.82 quality) rather than the
 * raw multi-megabyte original. A source over MAX_SOURCE_BYTES is rejected up
 * front so an oversized image can't exhaust the webview during decode.
 */
export async function downscaleImage(
  file: Blob,
  maxEdge: number,
  quality: number,
): Promise<string> {
  if (file.size > MAX_SOURCE_BYTES) {
    throw new Error(
      `image is too large (${Math.round(file.size / (1024 * 1024))} MB); the limit is ${
        MAX_SOURCE_BYTES / (1024 * 1024)
      } MB`,
    );
  }
  const canvas = await decodeScaled(file, maxEdge);
  return canvas.toDataURL("image/jpeg", clamp(quality, 0, 1));
}

/** A small preview of a wallpaper data URL for theme cards (default ~320px
 *  edge). Same pipeline as downscaleImage but takes an existing data URL. */
export async function makeThemeThumbnail(
  imageDataUrl: string,
  maxEdge = 320,
): Promise<string> {
  const img = await loadImage(imageDataUrl);
  const canvas = drawScaled(img, maxEdge);
  return canvas.toDataURL("image/jpeg", 0.8);
}

// ---- median-cut quantization ----------------------------------------------

interface Bucket {
  color: Rgb;
  count: number;
}

/** Split pixels into 2^depth buckets by repeatedly cutting the longest color
 *  channel at its median, then average each bucket. Returns buckets with their
 *  population, biggest first. Empty buckets are dropped. */
function medianCut(pixels: Rgb[], depth: number): Bucket[] {
  if (pixels.length === 0) return [];
  let groups: Rgb[][] = [pixels];
  for (let d = 0; d < depth; d++) {
    const next: Rgb[][] = [];
    for (const group of groups) {
      if (group.length <= 1) {
        next.push(group);
        continue;
      }
      // Widest channel decides the cut axis.
      let rMin = 255,
        gMin = 255,
        bMin = 255,
        rMax = 0,
        gMax = 0,
        bMax = 0;
      for (const p of group) {
        if (p.r < rMin) rMin = p.r;
        if (p.g < gMin) gMin = p.g;
        if (p.b < bMin) bMin = p.b;
        if (p.r > rMax) rMax = p.r;
        if (p.g > gMax) gMax = p.g;
        if (p.b > bMax) bMax = p.b;
      }
      const rRange = rMax - rMin;
      const gRange = gMax - gMin;
      const bRange = bMax - bMin;
      const channel: keyof Rgb =
        rRange >= gRange && rRange >= bRange ? "r" : gRange >= bRange ? "g" : "b";
      group.sort((a, b) => a[channel] - b[channel]);
      const mid = group.length >> 1;
      next.push(group.slice(0, mid), group.slice(mid));
    }
    groups = next;
  }
  const buckets: Bucket[] = [];
  for (const group of groups) {
    if (group.length === 0) continue;
    let r = 0,
      g = 0,
      b = 0;
    for (const p of group) {
      r += p.r;
      g += p.g;
      b += p.b;
    }
    const n = group.length;
    buckets.push({
      color: { r: r / n, g: g / n, b: b / n },
      count: n,
    });
  }
  buckets.sort((a, b) => b.count - a.count);
  return buckets;
}

/** Squared RGB distance, for merging near-identical swatches. */
function dist2(a: Rgb, b: Rgb): number {
  const dr = a.r - b.r;
  const dg = a.g - b.g;
  const db = a.b - b.b;
  return dr * dr + dg * dg + db * db;
}

/** Fold buckets whose colors sit within minDist of an already-kept bucket into
 *  that bucket (summing coverage), so the swatch list has no visual duplicates. */
function mergeBuckets(buckets: Bucket[], minDist: number): Bucket[] {
  const kept: Bucket[] = [];
  const min2 = minDist * minDist;
  for (const b of buckets) {
    const near = kept.find((k) => dist2(k.color, b.color) < min2);
    if (near) near.count += b.count;
    else kept.push({ ...b });
  }
  kept.sort((a, b) => b.count - a.count);
  return kept;
}

// ---- suggestion building ---------------------------------------------------

interface Hsl {
  h: number;
  s: number;
  l: number;
}

function hexToHsl(hex: string): Hsl {
  return rgbToHsl(parseHex(hex) ?? { r: 0, g: 0, b: 0 });
}

function hslHex(h: number, s: number, l: number): string {
  return toHex(hslToRgb(h, clamp(s, 0, 1), clamp(l, 0, 1)));
}

/** An rgba() string from a hex + alpha, matching how the presets express the
 *  --line vars (a tint of the text color at low alpha). */
function rgba(hex: string, alpha: number): string {
  const { r, g, b } = parseHex(hex) ?? { r: 255, g: 255, b: 255 };
  return `rgba(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)}, ${alpha})`;
}

/**
 * Turn the dominant swatches into a full palette suggestion.
 *  - accent: the most saturated swatch whose lightness sits in a usable band
 *    (0.35..0.75); falls back to the overall most saturated, then the top
 *    swatch, so there is always an accent.
 *  - base: "light" when the image is bright on average (>0.6 lightness), else
 *    "dark" (kept deliberately simple per the brief).
 *  - neutral vars: a coherent ramp built from the dominant hue and the accent
 *    hue, dark or light family to match base. bg is the darkest/lightest anchor;
 *    panels step off it; text is near-white / near-black faintly tinted toward
 *    the accent; dim/faint step down; line/line-2 are low-alpha text tints.
 */
function buildSuggestion(swatches: string[], avgLightness: number): PaletteSuggestion {
  const hsls = swatches.map(hexToHsl);
  const light = avgLightness > 0.6;
  const base = light ? "light" : "dark";

  // Accent: most saturated in the usable lightness band.
  let accentIdx = -1;
  let accentScore = -1;
  for (let i = 0; i < hsls.length; i++) {
    const { s, l } = hsls[i];
    if (l < 0.35 || l > 0.75) continue;
    if (s > accentScore) {
      accentScore = s;
      accentIdx = i;
    }
  }
  if (accentIdx < 0) {
    // Fall back to the overall most saturated swatch.
    for (let i = 0; i < hsls.length; i++) {
      if (hsls[i].s > accentScore) {
        accentScore = hsls[i].s;
        accentIdx = i;
      }
    }
  }
  if (accentIdx < 0) accentIdx = 0;
  const accentHsl = hsls[accentIdx] ?? { h: 40, s: 0.55, l: 0.55 };
  // Nudge a washed-out or extreme accent into a readable range.
  const accent = hslHex(
    accentHsl.h,
    clamp(accentHsl.s, 0.4, 0.9),
    clamp(accentHsl.l, 0.4, 0.7),
  );

  // Neutral hue: the darkest (dark family) or lightest (light family) swatch's
  // hue, so the surfaces carry a faint cast from the image rather than pure grey.
  const anchor = [...hsls].sort((a, b) => (light ? b.l - a.l : a.l - b.l))[0] ?? {
    h: accentHsl.h,
    s: 0.2,
    l: light ? 0.9 : 0.08,
  };
  const nH = anchor.h;
  const nS = clamp(anchor.s, 0.08, 0.35); // keep surfaces muted, never vivid

  let colors: Record<NeutralVarKey, string>;
  if (light) {
    const text = hslHex(accentHsl.h, 0.25, 0.14);
    colors = {
      bg: hslHex(nH, nS, 0.95),
      "bg-raise": hslHex(nH, nS, 0.9),
      panel: hslHex(nH, nS * 0.5, 0.99),
      "panel-2": hslHex(nH, nS, 0.96),
      "panel-3": hslHex(nH, nS, 0.92),
      line: rgba(text, 0.12),
      "line-2": rgba(text, 0.2),
      text,
      dim: hslHex(accentHsl.h, 0.18, 0.36),
      faint: hslHex(accentHsl.h, 0.14, 0.55),
    };
  } else {
    const text = hslHex(accentHsl.h, 0.15, 0.9);
    colors = {
      bg: hslHex(nH, nS, 0.07),
      "bg-raise": hslHex(nH, nS, 0.1),
      panel: hslHex(nH, nS, 0.12),
      "panel-2": hslHex(nH, nS, 0.16),
      "panel-3": hslHex(nH, nS, 0.21),
      line: rgba(text, 0.09),
      "line-2": rgba(text, 0.18),
      text,
      dim: hslHex(accentHsl.h, 0.12, 0.62),
      faint: hslHex(accentHsl.h, 0.1, 0.42),
    };
  }

  return { base, accent, colors };
}

// ---- public entry point ----------------------------------------------------

/**
 * Extract a dominant palette + theme suggestion from a wallpaper data URL.
 * Draws the image to a 64px canvas, reads pixels, quantizes with median cut to
 * dominant colors, merges near-duplicates, then builds the suggestion. Pure
 * aside from the canvas read, and deterministic for a given image.
 */
export async function extractPalette(imageDataUrl: string): Promise<ExtractedPalette> {
  const img = await loadImage(imageDataUrl);
  const canvas = drawScaled(img, 64);
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("themeCraft: 2d canvas context unavailable");
  const { data } = ctx.getImageData(0, 0, canvas.width, canvas.height);

  const pixels: Rgb[] = [];
  let lSum = 0;
  for (let i = 0; i < data.length; i += 4) {
    const a = data[i + 3];
    if (a < 8) continue; // skip effectively-transparent pixels
    const r = data[i];
    const g = data[i + 1];
    const b = data[i + 2];
    pixels.push({ r, g, b });
    lSum += rgbToHsl({ r, g, b }).l;
  }
  const avgLightness = pixels.length ? lSum / pixels.length : 0;

  // depth 3 -> up to 8 buckets; merge visual near-duplicates (~7% of the RGB
  // cube apart) so the swatch list stays crisp. 6-10 swatches per the brief.
  const merged = mergeBuckets(medianCut(pixels, 3), 18);
  const swatches = merged.slice(0, 10).map((b) => toHex(b.color));
  const safeSwatches = swatches.length ? swatches : ["#0b0d12"];

  return {
    swatches: safeSwatches,
    suggestion: buildSuggestion(safeSwatches, avgLightness),
  };
}
