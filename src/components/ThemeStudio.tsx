// The Appearance section's theme machinery: a gallery of preset + custom theme
// cards, the accent / font rows beside them, and the Theme Studio editor that
// crafts a custom theme (base palette, accent, wallpaper, per-slot neutral
// colors) with a live preview. Everything talks to appearance.ts (which owns
// the DOM application) and the store (which owns the custom-theme records).
//
// ThemeSync is a headless bridge: it re-applies whichever custom theme the
// stored customThemeId points at whenever the records change (app boot,
// cross-window edits) and falls back to the presets when that id no longer
// resolves (deleted on another window).

import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { createPortal } from "react-dom";
import {
  ACCENTS,
  BG_DIM_MAX,
  BG_DIM_MIN,
  BG_PAN_MAX,
  BG_PAN_MIN,
  BG_ZOOM_MAX,
  BG_ZOOM_MIN,
  bgMaxShiftPct,
  bgTranslatePct,
  clamp,
  getAppearance,
  monoFontStack,
  NEUTRAL_VAR_KEYS,
  reapplyWithCustomTheme,
  setAppearance,
  THEMES,
  uiFontStack,
  ZOOM_MAX,
  ZOOM_MIN,
  ZOOM_STEP,
  APPEARANCE_EVENT,
  type Appearance,
} from "../lib/appearance";
import { downscaleImage, extractPalette } from "../lib/themeCraft";
import type { AiPalette, CustomTheme } from "../lib/protocol";
import { useStore } from "../state/store";
import { ColorPicker } from "./ColorPicker";
import { FontPicker } from "./FontPicker";
import { PencilIcon, PlusIcon, TrashIcon, XIcon } from "./icons";

// ---- small shared helpers --------------------------------------------------

/** Resolve an accent (a preset id or a "#rrggbb") to a display hex. */
function accentHex(accent: string): string {
  const preset = ACCENTS.find((p) => p.id === accent);
  if (preset) return preset.base;
  return /^#[0-9a-f]{6}$/i.test(accent) ? accent : ACCENTS[0].base;
}

/** True when the accent is a hand-picked hex rather than one of the presets. */
function isCustomAccent(accent: string): boolean {
  return !ACCENTS.some((p) => p.id === accent);
}

/** The swatch stack a card shows: a custom theme's own neutrals when it has
 *  them, otherwise its base preset's preview colors. */
function cardSwatches(theme: CustomTheme): string[] {
  const base = THEMES.find((t) => t.id === theme.base) ?? THEMES[0];
  return [
    theme.colors?.bg ?? base.preview.bg,
    theme.colors?.panel ?? base.preview.panel,
    accentHex(theme.accent),
  ];
}

/** Two-step confirm delete, the DangerButton idiom from Sidebar.tsx cloned here
 *  to avoid a Sidebar -> SettingsPopover -> ThemeStudio import cycle. */
function ThemeDeleteButton({ label, onConfirm }: { label: string; onConfirm: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 2600);
    return () => clearTimeout(t);
  }, [armed]);
  return (
    <button
      type="button"
      className={`icon-btn danger-btn${armed ? " armed" : ""}`}
      aria-label={label}
      title={armed ? "Click again to confirm" : label}
      onClick={(e) => {
        e.stopPropagation();
        if (armed) onConfirm();
        else setArmed(true);
      }}
    >
      {armed ? <span className="danger-confirm">sure?</span> : <TrashIcon size={13} />}
    </button>
  );
}

/** A row of the 9 accent presets + a custom-color swatch. Value is a preset id
 *  or a "#rrggbb"; clicking "custom" toggles the inline ColorPicker. */
function AccentSwatches({
  value,
  onPick,
}: {
  value: string;
  onPick: (accent: string) => void;
}) {
  const [picking, setPicking] = useState(false);
  const custom = isCustomAccent(value);
  return (
    <div className="theme-accent-picker">
      <div className="theme-accent-row">
        {ACCENTS.map((p) => (
          <button
            key={p.id}
            type="button"
            className={`theme-accent-dot${value === p.id ? " on" : ""}`}
            style={{ background: p.base }}
            title={p.label}
            aria-label={`Accent ${p.label}`}
            onClick={() => {
              setPicking(false);
              onPick(p.id);
            }}
          />
        ))}
        <button
          type="button"
          className={`theme-accent-dot custom${custom ? " on" : ""}`}
          style={custom ? { background: value } : undefined}
          title="Custom color"
          aria-label="Custom accent color"
          onClick={() => setPicking((v) => !v)}
        >
          {!custom && <PlusIcon size={11} />}
        </button>
      </div>
      {picking && (
        <div className="theme-picker-pop">
          <ColorPicker
            value={accentHex(value)}
            onChange={(c) => c && onPick(c)}
          />
        </div>
      )}
    </div>
  );
}

// ---- headless sync ---------------------------------------------------------

/**
 * Keeps the applied custom theme in step with server state. Runs whenever the
 * record list changes (boot load, a broadcast from another window): if the
 * stored customThemeId resolves to a record it is re-applied; if it resolves to
 * nothing (deleted elsewhere) we fall back to the presets. Idempotent, so it is
 * safe wherever it is mounted.
 *
 * NOTE: for this to cover APP BOOT (main.tsx applies the presets before the
 * records have loaded over the socket) it must live in an always-mounted
 * component. This file cannot touch App.tsx, so <ThemeSync/> is also mounted
 * inside the Settings screen for the settings-open case; App.tsx should mount
 * it once at the root for the boot + settings-closed case.
 */
export function ThemeSync() {
  const { state } = useStore();
  const themes = state.customThemes;
  const loaded = state.themesLoaded;
  useEffect(() => {
    // Until the first theme.list round trip lands, customThemes is the empty
    // seed — NOT an authoritative "the record is gone". Applying or clearing
    // off that seed is what unstuck a crafted theme on every boot: leave the
    // stored id alone until the records have actually loaded.
    if (!loaded) return;
    const a = getAppearance();
    if (!a.customThemeId) return; // presets active, nothing to resolve
    const record = themes.find((t) => t.id === a.customThemeId);
    if (record) reapplyWithCustomTheme(record);
    else setAppearance({ ...a, customThemeId: null }); // loaded & missing: fall back
  }, [themes, loaded]);
  return null;
}

// ---- the studio editor -----------------------------------------------------

interface Draft {
  name: string;
  base: string;
  accent: string;
  colors: Record<string, string>;
  backgroundImage?: string;
  backgroundDim: number;
  /** Wallpaper zoom (1..3) + placement (-100..100 stored units per axis). A
   *  fresh image opens at 1/0/0; editing loads the record's saved frame. */
  backgroundZoom: number;
  backgroundX: number;
  backgroundY: number;
}

function draftFrom(theme: CustomTheme | "new"): Draft {
  if (theme === "new") {
    const a = getAppearance();
    return {
      name: "",
      base: a.theme,
      accent: a.accent,
      colors: {},
      backgroundImage: undefined,
      backgroundDim: 0.35,
      backgroundZoom: 1,
      backgroundX: 0,
      backgroundY: 0,
    };
  }
  return {
    name: theme.name,
    base: theme.base,
    accent: theme.accent,
    colors: { ...(theme.colors ?? {}) },
    backgroundImage: theme.backgroundImage,
    // Clamp on load too: a hand-edited or corrupted record must not drive the
    // preview (or the drag math) with out-of-range values.
    backgroundDim: clamp(theme.backgroundDim ?? 0.35, BG_DIM_MIN, BG_DIM_MAX),
    backgroundZoom: clamp(theme.backgroundZoom ?? 1, BG_ZOOM_MIN, BG_ZOOM_MAX),
    backgroundX: clamp(theme.backgroundX ?? 0, BG_PAN_MIN, BG_PAN_MAX),
    backgroundY: clamp(theme.backgroundY ?? 0, BG_PAN_MIN, BG_PAN_MAX),
  };
}

/** Which neutral slot the inline picker is editing (accent has its own row). */
type NeutralKey = (typeof NEUTRAL_VAR_KEYS)[number];

/**
 * Create / edit a custom theme. Every change re-applies a draft record through
 * reapplyWithCustomTheme so the whole app previews it live (no persistence).
 * Cancel or unmount restores whatever was active on open; Save upserts through
 * the store and makes the saved theme the active one.
 */
export function ThemeStudioModal({
  editing,
  onClose,
}: {
  editing: CustomTheme | "new";
  onClose: () => void;
}) {
  const { state, actions } = useStore();
  const [draft, setDraft] = useState<Draft>(() => draftFrom(editing));
  const [slot, setSlot] = useState<NeutralKey | null>(null);
  const [swatches, setSwatches] = useState<string[] | null>(null);
  const [matching, setMatching] = useState(false); // instant local matcher busy
  const [generating, setGenerating] = useState(false); // AI palette in flight
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false); // panning the preview
  const [hint, setHint] = useState(false); // "zoom in to reposition" flash
  const fileRef = useRef<HTMLInputElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const hintTimer = useRef<number | undefined>(undefined);
  // Live pointer-drag bookkeeping: the captured pointer + last client coords, so
  // each move applies an incremental delta (see onPreviewPointerMove).
  const dragRef = useRef<{ pointerId: number; lastX: number; lastY: number } | null>(null);

  const existing = editing === "new" ? null : editing;
  const hasImage = !!draft.backgroundImage;
  const genBusy = matching || generating;

  // Whatever palette was on screen when the studio opened, so Cancel can put it
  // back. Captured once (undefined = not yet captured); the record list does not
  // change while we edit (Save is the only writer and it opts out of restore).
  const restoreRef = useRef(true);
  const priorRef = useRef<CustomTheme | null | undefined>(undefined);
  if (priorRef.current === undefined) {
    const a = getAppearance();
    priorRef.current = a.customThemeId
      ? (state.customThemes.find((t) => t.id === a.customThemeId) ?? null)
      : null;
  }

  // Guards for the slow (15-60s) AI generation: aliveRef flips false on unmount
  // so a late result never touches state after the studio closed; genRef is a
  // monotonic ticket so only the most recent request is allowed to apply (also
  // means a cancel/close mid-flight is ignored when it lands).
  const aliveRef = useRef(true);
  const genRef = useRef(0);

  // Build the record the preview / save use from the current draft.
  const record: CustomTheme = useMemo(
    () => ({
      id: existing?.id ?? "",
      name: draft.name.trim() || "Untitled theme",
      base: draft.base,
      accent: draft.accent,
      colors: Object.keys(draft.colors).length ? draft.colors : undefined,
      backgroundImage: draft.backgroundImage || undefined,
      backgroundDim: draft.backgroundImage ? draft.backgroundDim : undefined,
      backgroundZoom: draft.backgroundImage ? draft.backgroundZoom : undefined,
      backgroundX: draft.backgroundImage ? draft.backgroundX : undefined,
      backgroundY: draft.backgroundImage ? draft.backgroundY : undefined,
      createdAt: existing?.createdAt ?? new Date().toISOString(),
      updatedAt: existing?.updatedAt ?? new Date().toISOString(),
    }),
    [draft, existing],
  );

  // Live preview: re-apply on every draft change. Cheap: only inline vars +
  // the (already-downscaled) wallpaper url are rewritten.
  useEffect(() => {
    reapplyWithCustomTheme(record);
  }, [record]);

  // Restore the pre-open palette unless a Save took over; and mark unmounted so
  // an in-flight AI result can't apply after the studio is gone.
  useEffect(
    () => () => {
      aliveRef.current = false;
      window.clearTimeout(hintTimer.current);
      if (restoreRef.current) reapplyWithCustomTheme(priorRef.current ?? null);
    },
    [],
  );

  // Escape closes only the studio (capture phase so the settings screen stays),
  // but never while a save is in flight — the backend save cannot be cancelled,
  // so a late success must not land after the user thinks they dismissed it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      if (busy) return;
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose, busy]);

  /** Invalidate any in-flight generator (AI or matcher) so a palette computed
   *  from the PREVIOUS wallpaper can never overwrite the draft after the image
   *  was replaced or removed. Both generators capture this ticket at start and
   *  bail if it moved. */
  function invalidateGenerators() {
    genRef.current++;
  }

  async function chooseImage(file: File) {
    setError(null);
    try {
      const url = await downscaleImage(file, 1600, 0.82);
      invalidateGenerators();
      // A new image deserves a fresh frame: reset zoom + placement to defaults.
      setDraft((d) => ({ ...d, backgroundImage: url, backgroundZoom: 1, backgroundX: 0, backgroundY: 0 }));
      setSwatches(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function matchColors() {
    if (!draft.backgroundImage || genBusy) return;
    const ticket = ++genRef.current;
    setMatching(true);
    setError(null);
    try {
      const { swatches: sw, suggestion } = await extractPalette(draft.backgroundImage);
      // Fast, but the wallpaper can still change under a slow decode: honor the
      // same ticket guard the AI path uses so a stale result never applies.
      if (!aliveRef.current || genRef.current !== ticket) return;
      setSwatches(sw);
      setDraft((d) => ({
        ...d,
        base: suggestion.base,
        accent: suggestion.accent,
        colors: { ...suggestion.colors },
      }));
    } catch (e) {
      if (!aliveRef.current || genRef.current !== ticket) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      // Reset busy regardless of the ticket: a wallpaper change can invalidate
      // the ticket mid-flight, and only this (single, non-overlapping) run owns
      // the flag, so it must clear it or the generators stay disabled forever.
      if (aliveRef.current) setMatching(false);
    }
  }

  /** Fold an AI-designed palette into the draft, exactly like the instant
   *  matcher: the family picks the base preset (light -> "light"; dark -> keep a
   *  dark base, else "dark"), the accent + all 10 slots are applied, and the
   *  suggested name only fills an empty name field. */
  function applyAiPalette(p: AiPalette) {
    const currentIsDark = THEMES.find((t) => t.id === draft.base)?.dark ?? true;
    const base = p.family === "light" ? "light" : currentIsDark ? draft.base : "dark";
    const colors: Record<string, string> = {};
    for (const key of NEUTRAL_VAR_KEYS) {
      const v = p.colors[key];
      if (typeof v === "string") colors[key] = v;
    }
    setSwatches([p.accent, ...NEUTRAL_VAR_KEYS.map((k) => p.colors[k])].filter(Boolean));
    setDraft((d) => ({
      ...d,
      base,
      accent: p.accent,
      colors,
      name: p.name && !d.name.trim() ? p.name : d.name,
    }));
  }

  async function aiScheme() {
    if (!draft.backgroundImage || genBusy) return;
    const ticket = ++genRef.current;
    setGenerating(true);
    setError(null);
    try {
      const palette = await actions.aiPalette(draft.backgroundImage);
      if (!aliveRef.current || genRef.current !== ticket) return; // stale / closed
      applyAiPalette(palette);
    } catch (e) {
      if (!aliveRef.current || genRef.current !== ticket) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      // Reset busy regardless of the ticket (see matchColors): a wallpaper
      // change invalidates the ticket mid-flight but this run still owns the
      // flag, so it must clear it even when its result is being discarded.
      if (aliveRef.current) setGenerating(false);
    }
  }

  async function save() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await actions.saveTheme(record);
      restoreRef.current = false; // keep the preview; it is real now
      setAppearance({ ...getAppearance(), customThemeId: saved.id });
      reapplyWithCustomTheme(saved);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  function setSlotColor(key: NeutralKey, color: string | null) {
    setDraft((d) => {
      const colors = { ...d.colors };
      if (color) colors[key] = color;
      else delete colors[key]; // cleared: fall back to the base palette
      return { ...d, colors };
    });
  }

  // ---- wallpaper placement (drag to pan, wheel/slider to zoom) --------------
  // The preview's image layer carries the SAME transform the real wallpaper does
  // (scale(z) translate(tx%, ty%) via bgTranslatePct), so panning here is honest:
  // it moves the exact same fraction of the exact same slack.
  const canPan = hasImage && draft.backgroundZoom > 1;
  const isDefaultFrame =
    draft.backgroundZoom === 1 && draft.backgroundX === 0 && draft.backgroundY === 0;

  /** Restore the neutral frame: plain cover, centered. */
  function resetFrame() {
    setDraft((d) => ({ ...d, backgroundZoom: 1, backgroundX: 0, backgroundY: 0 }));
  }

  /** Set zoom (slider). Snapping back to 1 recenters, so there is never a hidden
   *  pan the user cannot see or undo (the engine gives zoom 1 no slack anyway). */
  function setZoom(next: number) {
    const z = clamp(+next.toFixed(2), BG_ZOOM_MIN, BG_ZOOM_MAX);
    setDraft((d) =>
      z <= BG_ZOOM_MIN
        ? { ...d, backgroundZoom: BG_ZOOM_MIN, backgroundX: 0, backgroundY: 0 }
        : { ...d, backgroundZoom: z },
    );
  }

  /** Briefly surface "zoom in to reposition" when a drag is attempted with no
   *  slack. Self-clearing; the timer is also cleared on unmount. */
  function flashHint() {
    setHint(true);
    window.clearTimeout(hintTimer.current);
    hintTimer.current = window.setTimeout(() => setHint(false), 1800);
  }

  function onPreviewPointerDown(e: ReactPointerEvent<HTMLDivElement>) {
    // No slack at zoom 1 (cover already fills), so a drag would do nothing.
    if (draft.backgroundZoom <= 1) {
      flashHint();
      return;
    }
    const el = previewRef.current;
    if (!el) return;
    el.setPointerCapture(e.pointerId);
    dragRef.current = { pointerId: e.pointerId, lastX: e.clientX, lastY: e.clientY };
    setDragging(true);
    e.preventDefault(); // no text/image selection while dragging
  }

  function onPreviewPointerMove(e: ReactPointerEvent<HTMLDivElement>) {
    const d = dragRef.current;
    const el = previewRef.current;
    if (!d || !el || d.pointerId !== e.pointerId) return;
    const z = draft.backgroundZoom;
    const shiftPct = bgMaxShiftPct(z); // element-percent of slack per axis
    // Track the pointer even when there is no slack (wheel can drop zoom to 1
    // mid-drag): otherwise travel during the inert stretch accumulates into
    // the first delta after zooming back in and the wallpaper jumps.
    const dxPx = e.clientX - d.lastX;
    const dyPx = e.clientY - d.lastY;
    d.lastX = e.clientX;
    d.lastY = e.clientY;
    if (shiftPct <= 0) return;
    const w = el.clientWidth;
    const h = el.clientHeight;
    // Invert the engine's on-screen shift (z * tx% * W/100) so the image follows
    // the cursor 1:1: dStored = dxPx / (W * z * shiftPct/100) * 100.
    const dStoredX = w > 0 ? (dxPx / ((w * z * shiftPct) / 100)) * 100 : 0;
    const dStoredY = h > 0 ? (dyPx / ((h * z * shiftPct) / 100)) * 100 : 0;
    setDraft((dr) => ({
      ...dr,
      backgroundX: clamp(dr.backgroundX + dStoredX, BG_PAN_MIN, BG_PAN_MAX),
      backgroundY: clamp(dr.backgroundY + dStoredY, BG_PAN_MIN, BG_PAN_MAX),
    }));
  }

  function endPreviewDrag(e: ReactPointerEvent<HTMLDivElement>) {
    const d = dragRef.current;
    if (!d || d.pointerId !== e.pointerId) return;
    try {
      previewRef.current?.releasePointerCapture(e.pointerId);
    } catch {
      /* pointer already released */
    }
    dragRef.current = null;
    setDragging(false);
  }

  // Wheel-to-zoom in 0.1 steps. Attached natively with { passive: false } because
  // React registers its synthetic onWheel as passive on the root, so a synthetic
  // handler's preventDefault would be ignored and the modal would scroll instead.
  useEffect(() => {
    const el = previewRef.current;
    if (!el || !hasImage) return;
    const onWheelNative = (e: WheelEvent) => {
      e.preventDefault();
      // Horizontal trackpad gestures carry deltaY 0; without this guard the
      // fallthrough below would read them as zoom-out.
      if (e.deltaY === 0) return;
      const dir = e.deltaY < 0 ? 1 : -1;
      setDraft((dr) => {
        if (!dr.backgroundImage) return dr;
        const z = clamp(+(dr.backgroundZoom + dir * 0.1).toFixed(2), BG_ZOOM_MIN, BG_ZOOM_MAX);
        if (z === dr.backgroundZoom) return dr;
        // Zooming back out to plain cover recenters (no hidden pan), matching
        // the slider path and resetFrame.
        if (z <= BG_ZOOM_MIN)
          return { ...dr, backgroundZoom: BG_ZOOM_MIN, backgroundX: 0, backgroundY: 0 };
        return { ...dr, backgroundZoom: z };
      });
    };
    el.addEventListener("wheel", onWheelNative, { passive: false });
    return () => el.removeEventListener("wheel", onWheelNative);
  }, [hasImage]);

  // The one-line note under the generators: running > "add a wallpaper first".
  const genHint = generating
    ? "Claude is looking at your wallpaper…"
    : !hasImage
      ? "add a wallpaper first"
      : "";

  // Every dismissal path is blocked while a save is in flight (the backend save
  // cannot be cancelled). The Save button keeps its own busy/disabled state.
  const requestClose = () => {
    if (!busy) onClose();
  };

  return createPortal(
    <div className="modal-backdrop" onClick={requestClose}>
      <div
        className="modal studio"
        role="dialog"
        aria-label={existing ? `Edit theme ${existing.name}` : "Craft a theme"}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <span>{existing ? "Edit theme" : "Craft a theme"}</span>
          <button className="icon-btn" aria-label="Close" disabled={busy} onClick={requestClose}>
            <XIcon size={14} />
          </button>
        </div>

        <div className="studio-body">
          {/* LEFT: identity + wallpaper */}
          <div className="studio-col studio-identity">
            <label className="studio-field">
              <span className="studio-label">name</span>
              <input
                className="studio-name-input"
                value={draft.name}
                autoFocus
                placeholder="My theme"
                onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))}
              />
            </label>

            <div className="studio-label">base palette</div>
            <div className="theme-base-row">
              {THEMES.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className={`theme-base-chip${draft.base === t.id ? " on" : ""}`}
                  onClick={() => setDraft((d) => ({ ...d, base: t.id }))}
                >
                  <span className="theme-base-swatches">
                    <span style={{ background: t.preview.bg }} />
                    <span style={{ background: t.preview.panel }} />
                    <span style={{ background: t.preview.accentDefault }} />
                  </span>
                  <span className="theme-base-name">{t.label}</span>
                </button>
              ))}
            </div>

            <div className="studio-label">accent</div>
            <AccentSwatches
              value={draft.accent}
              onPick={(accent) => setDraft((d) => ({ ...d, accent }))}
            />

            <div className="studio-label">wallpaper</div>
            <input
              ref={fileRef}
              type="file"
              accept="image/*"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) void chooseImage(f);
                e.target.value = "";
              }}
            />
            {draft.backgroundImage ? (
              <div className="studio-wall">
                {/* An interactive placement canvas. Three stacked layers mirror
                    the real wallpaper so the frame previews exactly what ships:
                    the image (bottom) carries the SAME scale+translate transform
                    the app applies via bgTranslatePct, the dim gradient sits over
                    it (:root[data-has-bg] body::before), and the 86%-opaque
                    palette surface caps it (:root[data-has-bg] .app). The overlays
                    are pointer-transparent so drag-to-pan lands on the box.
                    Perfect parity is impossible (16:10 box vs an arbitrary
                    viewport), but zoom + the fraction-of-slack pan match exactly. */}
                <div
                  ref={previewRef}
                  className={`studio-wall-preview${canPan ? " can-pan" : ""}${dragging ? " dragging" : ""}`}
                  onPointerDown={onPreviewPointerDown}
                  onPointerMove={onPreviewPointerMove}
                  onPointerUp={endPreviewDrag}
                  onPointerCancel={endPreviewDrag}
                  onDoubleClick={resetFrame}
                >
                  <div
                    className="studio-wall-img"
                    style={{
                      backgroundImage: `url("${draft.backgroundImage}")`,
                      transform: `scale(${draft.backgroundZoom}) translate(${bgTranslatePct(
                        draft.backgroundZoom,
                        draft.backgroundX,
                      )}%, ${bgTranslatePct(draft.backgroundZoom, draft.backgroundY)}%)`,
                    }}
                  />
                  <div
                    className="studio-wall-dim"
                    style={{ background: `rgba(0,0,0,${draft.backgroundDim})` }}
                  />
                  <div className="studio-wall-surface" />
                </div>
                <div className="studio-wall-links">
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => fileRef.current?.click()}
                  >
                    replace
                  </button>
                  <button
                    type="button"
                    className="link-btn"
                    onClick={() => {
                      invalidateGenerators();
                      // Drop the image AND its frame (zoom/placement) so a later
                      // image starts from a clean cover, not a stale crop.
                      setDraft((d) => ({
                        ...d,
                        backgroundImage: undefined,
                        backgroundZoom: 1,
                        backgroundX: 0,
                        backgroundY: 0,
                      }));
                      setSwatches(null);
                    }}
                  >
                    remove
                  </button>
                </div>
                <label className="studio-dim">
                  <span>dim</span>
                  <input
                    type="range"
                    min={BG_DIM_MIN}
                    max={BG_DIM_MAX}
                    step={0.02}
                    value={draft.backgroundDim}
                    onChange={(e) =>
                      setDraft((d) => ({ ...d, backgroundDim: Number(e.target.value) }))
                    }
                  />
                  <span className="studio-dim-val">
                    {Math.round(draft.backgroundDim * 100)}%
                  </span>
                </label>
                <label className="studio-dim studio-zoom">
                  <span>zoom</span>
                  <input
                    type="range"
                    min={BG_ZOOM_MIN}
                    max={BG_ZOOM_MAX}
                    step={0.05}
                    value={draft.backgroundZoom}
                    onChange={(e) => setZoom(Number(e.target.value))}
                  />
                  <span className="studio-dim-val">{draft.backgroundZoom.toFixed(1)}x</span>
                  {!isDefaultFrame && (
                    <button
                      type="button"
                      className="link-btn studio-frame-reset"
                      title="Reset zoom and position"
                      onClick={resetFrame}
                    >
                      reset
                    </button>
                  )}
                </label>
                {hint && <div className="studio-frame-hint">zoom in to reposition</div>}
              </div>
            ) : (
              <button
                type="button"
                className="studio-wall-empty"
                onClick={() => fileRef.current?.click()}
              >
                <PlusIcon size={16} />
                <span>choose image</span>
              </button>
            )}
          </div>

          {/* RIGHT: colour work */}
          <div className="studio-col studio-colors">
            <div className="studio-generators">
              <button
                type="button"
                className="studio-gen-btn"
                disabled={!hasImage || genBusy}
                title={!hasImage ? "add a wallpaper first" : "Match the palette to your image"}
                onClick={() => void matchColors()}
              >
                {matching && <span className="studio-spinner" aria-hidden="true" />}
                {matching ? "matching…" : "match colors"}
              </button>
              <button
                type="button"
                className="studio-gen-btn ai"
                disabled={!hasImage || genBusy}
                title={!hasImage ? "add a wallpaper first" : "Design a palette with Claude"}
                onClick={() => void aiScheme()}
              >
                {generating && <span className="studio-spinner" aria-hidden="true" />}
                {generating ? "designing…" : "AI color scheme"}
              </button>
            </div>
            {genHint && <div className="studio-gen-hint">{genHint}</div>}

            {swatches && (
              <div className="studio-swatch-strip" aria-label="Palette colors">
                {swatches.map((c, i) => (
                  <span key={`${c}-${i}`} style={{ background: c }} title={c} />
                ))}
              </div>
            )}

            <div className="studio-label">palette colors</div>
            <div className="studio-slot-grid">
              {NEUTRAL_VAR_KEYS.map((key) => {
                const set = draft.colors[key];
                const on = slot === key;
                return (
                  <div key={key} className={`studio-slot${on ? " editing" : ""}`}>
                    <button
                      type="button"
                      className={`studio-slot-swatch${set ? "" : " auto"}`}
                      style={set ? { background: set } : undefined}
                      title={set ?? "auto (from base palette)"}
                      aria-label={`Edit ${key} color`}
                      onClick={() => setSlot(on ? null : key)}
                    />
                    <span className="studio-slot-body">
                      <span className="studio-slot-name">{key}</span>
                      <span className="studio-slot-state">{set ?? "auto"}</span>
                    </span>
                    {set && (
                      <button
                        type="button"
                        className="link-btn studio-slot-reset"
                        title="Reset to the base palette"
                        onClick={() => setSlotColor(key, null)}
                      >
                        auto
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
            {slot && (
              <div className="theme-picker-pop">
                <ColorPicker
                  value={draft.colors[slot] ?? null}
                  onChange={(c) => setSlotColor(slot, c)}
                />
              </div>
            )}
          </div>
        </div>

        {error && <div className="modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn tone-deny" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn tone-allow"
            disabled={busy || !draft.name.trim()}
            title={draft.name.trim() ? undefined : "Name the theme to save it"}
            onClick={() => void save()}
          >
            {busy ? "Saving…" : "Save theme"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

// ---- the appearance section (gallery + accent + fonts + zoom) --------------

/**
 * The whole Settings -> Appearance body above the sidebar block: the theme
 * gallery, the accent row, interface + code font rows, and interface zoom. Owns
 * a single Appearance state so all four stay in step, and re-reads it on the
 * APPEARANCE_EVENT so a Studio save or a ThemeSync fallback is reflected here.
 */
export function AppearanceStudio() {
  const { state, actions } = useStore();
  const [a, setA] = useState<Appearance>(getAppearance);
  const [studio, setStudio] = useState<CustomTheme | "new" | null>(null);

  // A Studio save (or ThemeSync clearing a deleted id) writes appearance and
  // fires this event; mirror it so the active ring and accent row update.
  useEffect(() => {
    const onEvt = () => setA(getAppearance());
    window.addEventListener(APPEARANCE_EVENT, onEvt);
    return () => window.removeEventListener(APPEARANCE_EVENT, onEvt);
  }, []);

  function update(next: Appearance) {
    setA(next);
    setAppearance(next);
  }

  function pickPreset(themeId: Appearance["theme"]) {
    // Clearing customThemeId here also drops any wallpaper, per the engine.
    update({ ...a, theme: themeId, customThemeId: null });
  }

  function pickCustom(theme: CustomTheme) {
    // Persist the id (keeps fonts/zoom), then apply the record's palette.
    setAppearance({ ...getAppearance(), customThemeId: theme.id });
    setA(getAppearance());
    reapplyWithCustomTheme(theme);
  }

  const custom = state.customThemes;
  const activeCustom = a.customThemeId
    ? custom.find((t) => t.id === a.customThemeId)
    : undefined;

  return (
    <div className="settings-block">
      <div className="settings-label">appearance</div>

      <div className="theme-gallery">
        {THEMES.map((t) => {
          const on = a.customThemeId === null && a.theme === t.id;
          return (
            <button
              key={t.id}
              type="button"
              className={`theme-card${on ? " on" : ""}`}
              aria-pressed={on}
              onClick={() => pickPreset(t.id)}
            >
              <span className="theme-card-swatches">
                <span style={{ background: t.preview.bg }} />
                <span style={{ background: t.preview.panel }} />
                <span style={{ background: t.preview.accentDefault }} />
              </span>
              <span className="theme-card-name">{t.label}</span>
            </button>
          );
        })}

        {custom.map((t) => {
          const on = a.customThemeId === t.id;
          const [bg, panel, accent] = cardSwatches(t);
          return (
            <div
              key={t.id}
              className={`theme-card custom${on ? " on" : ""}`}
              role="button"
              tabIndex={0}
              aria-pressed={on}
              onClick={() => pickCustom(t)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  pickCustom(t);
                }
              }}
            >
              {t.backgroundImage && (
                <span
                  className="theme-card-wall"
                  style={{ backgroundImage: `url("${t.backgroundImage}")` }}
                />
              )}
              <span className="theme-card-swatches">
                <span style={{ background: bg }} />
                <span style={{ background: panel }} />
                <span style={{ background: accent }} />
              </span>
              <span className="theme-card-name" title={t.name}>
                {t.name}
              </span>
              <span className="theme-card-actions">
                <button
                  type="button"
                  className="icon-btn"
                  aria-label={`Edit ${t.name}`}
                  title="Edit theme"
                  onClick={(e) => {
                    e.stopPropagation();
                    setStudio(t);
                  }}
                >
                  <PencilIcon size={13} />
                </button>
                <ThemeDeleteButton
                  label={`Delete ${t.name}`}
                  onConfirm={() => void actions.removeTheme(t.id).catch(() => undefined)}
                />
              </span>
            </div>
          );
        })}

        <button
          type="button"
          className="theme-card craft"
          onClick={() => setStudio("new")}
        >
          <PlusIcon size={18} />
          <span className="theme-card-name">craft a theme</span>
        </button>
      </div>

      {/* Accent: editable for presets; a custom theme carries its own, so the
          row points at the Studio instead of quietly editing appearance. */}
      <div className="settings-row theme-accent-line">
        <span className="settings-value">accent</span>
        {activeCustom ? (
          <span className="theme-accent-locked">
            <span className="settings-hint">this theme carries its own accent; edit the theme</span>
            <button
              type="button"
              className="settings-toggle"
              onClick={() => setStudio(activeCustom)}
            >
              edit theme
            </button>
          </span>
        ) : (
          <AccentSwatches value={a.accent} onPick={(accent) => update({ ...a, accent })} />
        )}
      </div>

      <div className="settings-row theme-font-row">
        <span className="settings-value">
          interface font
          <span className="theme-font-preview" style={{ fontFamily: uiFontStack(a.fontUi) }}>
            The quick brown fox
          </span>
        </span>
        <FontPicker kind="ui" value={a.fontUi} onChange={(id) => update({ ...a, fontUi: id })} />
      </div>
      <div className="settings-row theme-font-row">
        <span className="settings-value">
          code font
          <span className="theme-font-preview" style={{ fontFamily: monoFontStack(a.fontMono) }}>
            const x = 42;
          </span>
        </span>
        <FontPicker kind="mono" value={a.fontMono} onChange={(id) => update({ ...a, fontMono: id })} />
      </div>

      <ZoomRow value={a.uiZoom} onChange={(z) => update({ ...a, uiZoom: z })} />

      {studio && (
        <ThemeStudioModal editing={studio} onClose={() => setStudio(null)} />
      )}
    </div>
  );
}

/** Conversation zoom stepper (scales the message feed only), matching the
 *  Stepper idiom in SettingsPopover. */
function ZoomRow({ value, onChange }: { value: number; onChange: (z: number) => void }) {
  const step = (delta: number): void =>
    onChange(clamp(+(value + delta).toFixed(2), ZOOM_MIN, ZOOM_MAX));
  return (
    <div className="settings-row">
      <span className="settings-value">conversation zoom</span>
      <span className="settings-step">
        <button
          type="button"
          className="settings-step-btn"
          aria-label="Decrease conversation zoom"
          onClick={() => step(-ZOOM_STEP)}
          disabled={value <= ZOOM_MIN}
        >
          −
        </button>
        <span className="settings-step-val">{Math.round(value * 100)}%</span>
        <button
          type="button"
          className="settings-step-btn"
          aria-label="Increase conversation zoom"
          onClick={() => step(ZOOM_STEP)}
          disabled={value >= ZOOM_MAX}
        >
          +
        </button>
      </span>
    </div>
  );
}
