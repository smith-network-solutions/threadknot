import { useEffect, useRef, useState } from "react";

/** The 8 preset accent colors a machine can wear (pulled from the terminal
 *  palette so rings match the rest of the chrome). */
export const ACCENT_PRESETS = [
  "#d9a35c",
  "#43c9a5",
  "#6fa8e8",
  "#b98ce8",
  "#e0655f",
  "#e5b567",
  "#63e9c5",
  "#5c6478",
];

interface Rgb {
  r: number;
  g: number;
  b: number;
}
interface Hsv {
  /** 0-360 */
  h: number;
  /** 0-1 */
  s: number;
  /** 0-1 */
  v: number;
}

function clamp01(n: number): number {
  return Math.min(1, Math.max(0, n));
}

export function hexToRgb(hex: string): Rgb | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

export function rgbToHex({ r, g, b }: Rgb): string {
  return "#" + ((1 << 24) | (r << 16) | (g << 8) | b).toString(16).slice(1);
}

function rgbToHsv({ r, g, b }: Rgb): Hsv {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const d = max - min;
  let h = 0;
  if (d !== 0) {
    if (max === rn) h = ((gn - bn) / d) % 6;
    else if (max === gn) h = (bn - rn) / d + 2;
    else h = (rn - gn) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  return { h, s: max === 0 ? 0 : d / max, v: max };
}

function hsvToRgb({ h, s, v }: Hsv): Rgb {
  const c = v * s;
  const hp = (((h % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  let rn = 0;
  let gn = 0;
  let bn = 0;
  if (hp < 1) [rn, gn, bn] = [c, x, 0];
  else if (hp < 2) [rn, gn, bn] = [x, c, 0];
  else if (hp < 3) [rn, gn, bn] = [0, c, x];
  else if (hp < 4) [rn, gn, bn] = [0, x, c];
  else if (hp < 5) [rn, gn, bn] = [x, 0, c];
  else [rn, gn, bn] = [c, 0, x];
  const m = v - c;
  return {
    r: Math.round((rn + m) * 255),
    g: Math.round((gn + m) * 255),
    b: Math.round((bn + m) * 255),
  };
}

/** window.EyeDropper (Chromium; WebView2 has it). Feature-detected at render. */
interface EyeDropperResult {
  sRGBHex: string;
}
type EyeDropperCtor = new () => { open(): Promise<EyeDropperResult> };

function getEyeDropper(): EyeDropperCtor | null {
  const ctor = (window as { EyeDropper?: unknown }).EyeDropper;
  return typeof ctor === "function" ? (ctor as EyeDropperCtor) : null;
}

function EyeDropGlyph() {
  return (
    <svg
      width={14}
      height={14}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="m2 22 1-1h3l9-9" />
      <path d="M3 21v-3l9-9" />
      <path d="m15 6 3.4-3.4a2.1 2.1 0 1 1 3 3L18 9l.4.4a2.1 2.1 0 1 1-3 3l-3.8-3.8a2.1 2.1 0 1 1 3-3l.4.4Z" />
    </svg>
  );
}

/**
 * Full pointer-driven color picker, no dependencies: saturation/value square
 * (CSS gradients + drag handle), rainbow hue bar, current swatch, R/G/B
 * number inputs (synced both ways), optional eyedropper (only where the
 * Chromium EyeDropper API exists), the 8 preset swatches as a quick-pick row,
 * and a "no color" clear. Emits #rrggbb, or null for "no color".
 */
export function ColorPicker({
  value,
  onChange,
}: {
  value: string | null;
  onChange: (color: string | null) => void;
}) {
  const [hsv, setHsv] = useState<Hsv>(() => {
    const rgb = value ? hexToRgb(value) : null;
    return rgb ? rgbToHsv(rgb) : { h: 30, s: 0.58, v: 0.85 };
  });
  const hsvRef = useRef(hsv);
  hsvRef.current = hsv;
  /** Last hex WE emitted; external changes not matching it resync the wheel. */
  const lastEmitRef = useRef<string | null>(value ? value.toLowerCase() : null);
  const svDragRef = useRef<number | null>(null);
  const hueDragRef = useRef<number | null>(null);

  useEffect(() => {
    const norm = value ? value.toLowerCase() : null;
    if (norm === lastEmitRef.current) return;
    lastEmitRef.current = norm;
    if (!norm) return; // cleared: keep the wheel where it was
    const rgb = hexToRgb(norm);
    if (!rgb) return;
    const next = rgbToHsv(rgb);
    // Grays/whites carry no hue info: keep the current hue so the square
    // doesn't jump to red.
    setHsv((prev) => (next.s === 0 ? { ...next, h: prev.h } : next));
  }, [value]);

  function commit(next: Hsv) {
    setHsv(next);
    const hex = rgbToHex(hsvToRgb(next));
    lastEmitRef.current = hex;
    onChange(hex);
  }

  function commitHex(hex: string) {
    const rgb = hexToRgb(hex);
    if (!rgb) return;
    const next = rgbToHsv(rgb);
    setHsv((prev) => (next.s === 0 ? { ...next, h: prev.h } : next));
    const norm = rgbToHex(rgb);
    lastEmitRef.current = norm;
    onChange(norm);
  }

  function svFromPoint(el: HTMLElement, clientX: number, clientY: number) {
    const rect = el.getBoundingClientRect();
    const s = clamp01((clientX - rect.left) / rect.width);
    const v = 1 - clamp01((clientY - rect.top) / rect.height);
    commit({ ...hsvRef.current, s, v });
  }

  function hueFromPoint(el: HTMLElement, clientX: number) {
    const rect = el.getBoundingClientRect();
    const h = clamp01((clientX - rect.left) / rect.width) * 360;
    commit({ ...hsvRef.current, h });
  }

  const rgb = hsvToRgb(hsv);
  const hex = rgbToHex(rgb);
  const hueRgb = hsvToRgb({ h: hsv.h, s: 1, v: 1 });
  const hueHex = rgbToHex(hueRgb);
  const eyeDropper = getEyeDropper();

  function setChannel(ch: keyof Rgb, raw: string) {
    const n = Number(raw);
    if (!Number.isFinite(n)) return;
    const nextRgb = { ...rgb, [ch]: Math.min(255, Math.max(0, Math.round(n))) };
    commitHex(rgbToHex(nextRgb));
  }

  return (
    <div className="color-picker">
      <div
        className="cp-sv"
        role="slider"
        aria-label="Saturation and brightness"
        aria-valuetext={`saturation ${Math.round(hsv.s * 100)}%, brightness ${Math.round(hsv.v * 100)}%`}
        style={{
          background: `linear-gradient(to top, #000, transparent), linear-gradient(to right, #fff, ${hueHex})`,
        }}
        onPointerDown={(e) => {
          e.preventDefault();
          e.currentTarget.setPointerCapture(e.pointerId);
          svDragRef.current = e.pointerId;
          svFromPoint(e.currentTarget, e.clientX, e.clientY);
        }}
        onPointerMove={(e) => {
          if (svDragRef.current !== e.pointerId) return;
          svFromPoint(e.currentTarget, e.clientX, e.clientY);
        }}
        onPointerUp={() => (svDragRef.current = null)}
        onPointerCancel={() => (svDragRef.current = null)}
      >
        <span
          className="cp-sv-handle"
          style={{
            left: `${hsv.s * 100}%`,
            top: `${(1 - hsv.v) * 100}%`,
            background: hex,
          }}
        />
      </div>

      <div
        className="cp-hue"
        role="slider"
        aria-label="Hue"
        aria-valuemin={0}
        aria-valuemax={360}
        aria-valuenow={Math.round(hsv.h)}
        onPointerDown={(e) => {
          e.preventDefault();
          e.currentTarget.setPointerCapture(e.pointerId);
          hueDragRef.current = e.pointerId;
          hueFromPoint(e.currentTarget, e.clientX);
        }}
        onPointerMove={(e) => {
          if (hueDragRef.current !== e.pointerId) return;
          hueFromPoint(e.currentTarget, e.clientX);
        }}
        onPointerUp={() => (hueDragRef.current = null)}
        onPointerCancel={() => (hueDragRef.current = null)}
      >
        <span className="cp-hue-handle" style={{ left: `${(hsv.h / 360) * 100}%`, background: hueHex }} />
      </div>

      <div className="cp-row">
        <span
          className={`cp-swatch${value ? "" : " none"}`}
          style={value ? { background: value } : undefined}
          title={value ?? "no color"}
        />
        {eyeDropper && (
          <button
            type="button"
            className="icon-btn cp-eyedrop"
            title="Pick a color from the screen"
            aria-label="Pick a color from the screen"
            onClick={() => {
              // Chromium-only API; open() rejects when the user presses Esc.
              try {
                void new eyeDropper()
                  .open()
                  .then((res) => {
                    if (res?.sRGBHex) commitHex(res.sRGBHex);
                  })
                  .catch(() => undefined);
              } catch {
                // Constructor blew up (partial implementations): ignore.
              }
            }}
          >
            <EyeDropGlyph />
          </button>
        )}
        {(["r", "g", "b"] as const).map((ch) => (
          <label key={ch} className="cp-field">
            <input
              type="number"
              min={0}
              max={255}
              value={rgb[ch]}
              aria-label={`${ch.toUpperCase()} channel`}
              onChange={(e) => setChannel(ch, e.target.value)}
            />
            <span>{ch}</span>
          </label>
        ))}
      </div>

      <div className="color-swatches">
        {ACCENT_PRESETS.map((c) => (
          <button
            key={c}
            type="button"
            className={`color-swatch${value?.toLowerCase() === c ? " on" : ""}`}
            style={{ background: c }}
            title={c}
            aria-label={`Accent color ${c}`}
            onClick={() => commitHex(c)}
          />
        ))}
        <button
          type="button"
          className={`color-swatch clear${value ? "" : " on"}`}
          title="no color"
          aria-label="No accent color"
          onClick={() => {
            lastEmitRef.current = null;
            onChange(null);
          }}
        />
      </div>
    </div>
  );
}
