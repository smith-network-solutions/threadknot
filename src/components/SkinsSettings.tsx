// Settings -> Appearance -> skins. A theme is a palette; a SKIN is the whole
// cabinet treatment that rides on top of one (chrome, meters, dialogs, panes).
// Only Retro-Tech ships today, so this block is deliberately small: a curated
// card with apply/preview, the per-module on/off checklist once the skin is
// live, and a marketplace row that carries crafted themes in and out as JSON.
//
// The module toggles write through appearance.ts (getSkinPrefs/setSkinPrefs),
// which owns the documentElement dataset the CSS gates read. This file never
// touches the dataset itself.

import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  ACCENTS,
  APPEARANCE_EVENT,
  BG_DIM_MAX,
  BG_DIM_MIN,
  BG_PAN_MAX,
  BG_PAN_MIN,
  BG_ZOOM_MAX,
  BG_ZOOM_MIN,
  clamp,
  getAppearance,
  getSkinPrefs,
  setAppearance,
  setSkinPrefs,
  SKINPREFS_EVENT,
  THEMES,
  type Appearance,
  type SkinModule,
} from "../lib/appearance";
import {
  CURATED_SKINS,
  SKIN_MARKET_URL,
  SKIN_MODULES,
  type CuratedSkin,
} from "../lib/skins";
import type { CustomTheme } from "../lib/protocol";
import { useStore } from "../state/store";
import { XIcon } from "./icons";
import "../styles/skins.css";

/** Extra plain-language notes appended to a module's own hint. The cards line
 *  spells out that turning it off leaves the sidebar exactly as the user has
 *  it, which is the question people actually ask about this toggle. Skipped
 *  when the catalog hint already makes the same point, so the row never says
 *  the same thing twice. */
const MODULE_NOTES: Partial<Record<SkinModule, string>> = {
  cards:
    "Off means the skin will not theme sidebar cards: your current sidebar format stays exactly as it is.",
};

function moduleNote(id: SkinModule, hint: string): string {
  const note = MODULE_NOTES[id];
  if (!note) return "";
  return hint.toLowerCase().includes("theme sidebar cards") ? "" : ` ${note}`;
}

/** One-line summary shown on the card face (the long copy is the description
 *  line plus the native title tooltip). */
function skinTagline(skin: CuratedSkin): string {
  const first = skin.description.split(/(?<=\.)\s/)[0] ?? skin.description;
  return first.trim();
}

/** Whether the skin is the one currently on screen. The test is against the
 *  EFFECTIVE base, not the stored preset: a crafted theme borrows a preset's
 *  palette through its `base`, and applyAppearance resolves data-theme to that
 *  base, so a theme built on the skin's own preset is running the skin's CSS.
 *  Reading only Appearance.theme would call it inactive and hide the module
 *  toggles, leaving the user with a skin on and no switch to turn its parts off.
 *  A customThemeId with no record behind it resolves the same way the appearance
 *  module does: back to the preset. */
function isSkinActive(
  a: Appearance,
  custom: CustomTheme | undefined,
  skin: CuratedSkin,
): boolean {
  return (custom ? custom.base : a.theme) === skin.id;
}

/** The crafted theme a given Appearance points at, or undefined for presets. */
function findActiveCustom(
  a: Appearance,
  themes: readonly CustomTheme[],
): CustomTheme | undefined {
  return a.customThemeId ? themes.find((t) => t.id === a.customThemeId) : undefined;
}

/** Clamp an optional numeric field off an imported file into its allowed band.
 *  Absent (or unparseable) stays absent, so the record keeps its own default. */
function clampImported(value: unknown, lo: number, hi: number): number | undefined {
  if (value === undefined || value === null) return undefined;
  const n = Number(value);
  return Number.isFinite(n) ? clamp(n, lo, hi) : undefined;
}

/** Appearance state mirrored into React, re-read on APPEARANCE_EVENT so a
 *  Studio save or a preset pick elsewhere in the section updates these cards. */
function useAppearanceState(): Appearance {
  const [a, setA] = useState<Appearance>(getAppearance);
  useEffect(() => {
    const onEvt = () => setA(getAppearance());
    window.addEventListener(APPEARANCE_EVENT, onEvt);
    return () => window.removeEventListener(APPEARANCE_EVENT, onEvt);
  }, []);
  return a;
}

/** The disabled-module set, mirrored the same way. Returns a checked-state
 *  reader plus a setter so the modal and the inline list stay in step: both
 *  write through setSkinPrefs, which fires SKINPREFS_EVENT back at the other. */
function useSkinModules() {
  const [off, setOff] = useState<SkinModule[]>(() => getSkinPrefs().off);
  useEffect(() => {
    const onEvt = () => setOff(getSkinPrefs().off);
    window.addEventListener(SKINPREFS_EVENT, onEvt);
    return () => window.removeEventListener(SKINPREFS_EVENT, onEvt);
  }, []);
  const isOn = useCallback((id: SkinModule) => !off.includes(id), [off]);
  const toggle = useCallback((id: SkinModule) => {
    const cur = getSkinPrefs().off;
    const next = cur.includes(id) ? cur.filter((m) => m !== id) : [...cur, id];
    setSkinPrefs({ off: next });
    setOff(next);
  }, []);
  return { isOn, toggle };
}

/** The shared module checklist. `variant` only picks up extra class names so
 *  the modal copy can breathe more than the inline copy. */
function SkinModuleList({ variant }: { variant: "inline" | "preview" }) {
  const { isOn, toggle } = useSkinModules();
  return (
    <div
      className={`skin-modules ${
        variant === "preview" ? "skin-preview-modules" : "skin-modules-inline"
      }`}
    >
      {SKIN_MODULES.map((m) => {
        const on = isOn(m.id);
        return (
          <div className="skin-module-row" key={m.id}>
            <span className="skin-module-text">
              <span className="skin-module-label">{m.label}</span>
              <span className="skin-module-hint">
                {m.hint}
                {moduleNote(m.id, m.hint)}
              </span>
            </span>
            <span className="settings-seg skin-module-seg">
              <button
                type="button"
                className={`settings-toggle skin-module-toggle ${on ? "on" : ""}`}
                aria-pressed={on}
                onClick={() => toggle(m.id)}
              >
                {on ? "on" : "off"}
              </button>
            </span>
          </div>
        );
      })}
    </div>
  );
}

/**
 * Full-bleed preview: a screenshot carousel with the skin's long description,
 * the module checklist and an Apply button. Portaled to <body> like the other
 * modals so the settings screen's own transform cannot capture it.
 */
function SkinPreviewModal({
  skin,
  active,
  onApply,
  onClose,
}: {
  skin: CuratedSkin;
  active: boolean;
  onApply: () => void;
  onClose: () => void;
}) {
  const [i, setI] = useState(0);
  const shots = skin.screenshots;
  const count = shots.length;
  const step = useCallback(
    (delta: number) => {
      if (count < 2) return;
      setI((prev) => (prev + delta + count) % count);
    },
    [count],
  );

  // Escape closes, arrows page the carousel: same key handling as the other
  // portaled dialogs (see ReviewMenu's ReviewDialog).
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowRight") step(1);
      else if (e.key === "ArrowLeft") step(-1);
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, step]);

  const shot = shots[Math.min(i, Math.max(count - 1, 0))];

  return createPortal(
    <div className="modal-backdrop skin-preview-backdrop" onClick={onClose}>
      <div
        className="modal skin-preview"
        role="dialog"
        aria-label={`${skin.name} preview`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head skin-preview-head">
          <span className="skin-preview-title">{skin.name}</span>
          <button className="icon-btn" aria-label="Close preview" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        {shot && (
          <>
            <div className="skin-preview-stage">
              <img className="skin-preview-img" src={shot.src} alt={shot.caption} />
              {count > 1 && (
                <>
                  <button
                    type="button"
                    className="skin-preview-nav prev"
                    aria-label="Previous screenshot"
                    onClick={() => step(-1)}
                  >
                    ‹
                  </button>
                  <button
                    type="button"
                    className="skin-preview-nav next"
                    aria-label="Next screenshot"
                    onClick={() => step(1)}
                  >
                    ›
                  </button>
                </>
              )}
            </div>
            <div className="skin-preview-caption">{shot.caption}</div>
            {count > 1 && (
              <div className="skin-preview-dots">
                {shots.map((s, n) => (
                  <button
                    key={s.src}
                    type="button"
                    className={`skin-preview-dot ${n === i ? "on" : ""}`}
                    aria-label={`Screenshot ${n + 1}`}
                    aria-current={n === i}
                    onClick={() => setI(n)}
                  />
                ))}
              </div>
            )}
          </>
        )}

        <p className="skin-preview-desc">{skin.description}</p>

        <div className="settings-label skin-preview-modules-label">what it changes</div>
        <SkinModuleList variant="preview" />

        <div className="modal-actions skin-preview-actions">
          <button type="button" className="settings-toggle" onClick={onClose}>
            close
          </button>
          <button
            type="button"
            className="settings-toggle primary"
            disabled={active}
            title={active ? "This skin is already applied." : undefined}
            onClick={onApply}
          >
            {active ? "applied" : "apply skin"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

/** Filename-safe slug for the exported theme file. */
function slug(name: string): string {
  const s = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
  return s || "theme";
}

/** Marketplace row: the outbound link plus the crafted-theme JSON round trip.
 *  Export writes whatever crafted theme is currently applied; import saves a
 *  file back through the store (id blanked so the server mints a fresh one). */
function SkinMarketRow({ a }: { a: Appearance }) {
  const { state, actions } = useStore();
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const activeCustom = findActiveCustom(a, state.customThemes);

  function exportTheme() {
    if (!activeCustom) return;
    setError(null);
    const blob = new Blob([JSON.stringify(activeCustom, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${slug(activeCustom.name)}.threadknot-theme.json`;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    // Revoke on the next tick: Safari needs the URL to survive the click.
    setTimeout(() => URL.revokeObjectURL(url), 0);
    setNote(`exported ${activeCustom.name}`);
  }

  /** A theme file is a stranger's JSON, so every field is checked before it is
   *  saved. The base is the one that has to be rejected outright: it picks both
   *  the palette and the light/dark family, so a bogus id would land the theme
   *  on the default palette with nothing to explain why. The rest degrade
   *  quietly (a junk accent becomes the default, the numbers are clamped to the
   *  same bands the Studio's own sliders use), and the record is rebuilt field
   *  by field so nothing else in the file rides along into storage. */
  async function importTheme(file: File) {
    setNote(null);
    setError(null);
    try {
      const raw = JSON.parse(await file.text()) as Partial<CustomTheme>;
      if (typeof raw.name !== "string" || typeof raw.base !== "string") {
        throw new Error("not a Threadknot theme file (needs a name and a base)");
      }
      const base = raw.base;
      if (!THEMES.some((t) => t.id === base)) {
        throw new Error(`unknown base theme ${base}`);
      }
      const accent = raw.accent;
      const record: CustomTheme = {
        id: "", // blank: the server mints the id, so an import never overwrites
        name: raw.name,
        base,
        accent:
          typeof accent === "string" &&
          (ACCENTS.some((c) => c.id === accent) || /^#[0-9a-fA-F]{6}$/.test(accent))
            ? accent
            : "brass",
        colors:
          raw.colors && typeof raw.colors === "object" ? { ...raw.colors } : undefined,
        backgroundImage:
          typeof raw.backgroundImage === "string" ? raw.backgroundImage : undefined,
        backgroundDim: clampImported(raw.backgroundDim, BG_DIM_MIN, BG_DIM_MAX),
        backgroundZoom: clampImported(raw.backgroundZoom, BG_ZOOM_MIN, BG_ZOOM_MAX),
        backgroundX: clampImported(raw.backgroundX, BG_PAN_MIN, BG_PAN_MAX),
        backgroundY: clampImported(raw.backgroundY, BG_PAN_MIN, BG_PAN_MAX),
        createdAt: typeof raw.createdAt === "string" ? raw.createdAt : "",
        updatedAt: typeof raw.updatedAt === "string" ? raw.updatedAt : "",
      };
      const saved = await actions.saveTheme(record);
      setNote(`imported ${saved.name}: pick it in the theme gallery above`);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <div className="skin-market">
      <div className="skin-market-blurb">
        Skins and crafted themes travel as plain files. Browse the community
        marketplace, or move a theme you built between machines.
      </div>
      <div className="skin-market-actions">
        <button
          type="button"
          className="settings-toggle"
          onClick={() => window.open(SKIN_MARKET_URL, "_blank", "noopener")}
        >
          browse marketplace
        </button>
        <button
          type="button"
          className="settings-toggle"
          disabled={!activeCustom}
          title={
            activeCustom
              ? `Download ${activeCustom.name} as JSON.`
              : "Apply a theme you crafted in the Theme Studio first: presets and skins are built in, so there is nothing to export."
          }
          onClick={exportTheme}
        >
          export theme
        </button>
        <button
          type="button"
          className="settings-toggle"
          title="Load a .threadknot-theme.json file into your crafted themes."
          onClick={() => fileRef.current?.click()}
        >
          import
        </button>
        <input
          ref={fileRef}
          className="skin-import-input"
          type="file"
          accept=".json,application/json"
          onChange={(e) => {
            const file = e.target.files?.[0];
            e.target.value = ""; // let the same file be picked twice in a row
            if (file) void importTheme(file);
          }}
        />
      </div>
      {note && <div className="settings-hint skin-market-note">{note}</div>}
      {error && <div className="settings-hint skin-market-error">{error}</div>}
    </div>
  );
}

/** The skins block itself: curated cards, live module checklist, marketplace. */
export function SkinsSettings() {
  const a = useAppearanceState();
  // The crafted themes come from the store, so a save or a delete elsewhere
  // re-renders this block on its own; the APPEARANCE_EVENT above covers the
  // other half (which theme is picked). Between them the active check tracks
  // whatever the DOM is actually wearing.
  const { state } = useStore();
  const activeCustom = findActiveCustom(a, state.customThemes);
  const [preview, setPreview] = useState<CuratedSkin | null>(null);

  function applySkin(skin: CuratedSkin) {
    // A skin rides on its own preset palette, so clearing customThemeId is part
    // of applying it (same rule the theme gallery uses when picking a preset).
    setAppearance({
      ...getAppearance(),
      theme: skin.id as Appearance["theme"],
      customThemeId: null,
    });
  }

  return (
    <div className="settings-block skins-block">
      <div className="settings-label">skins</div>
      <div className="settings-hint skins-intro">
        A theme sets the colours. A skin re-dresses the whole cabinet: chrome,
        meters, dialogs and panes. Apply one, then switch off any part you would
        rather keep plain.
      </div>

      <div className="skin-list">
        {CURATED_SKINS.map((skin) => {
          const active = isSkinActive(a, activeCustom, skin);
          return (
            <div
              key={skin.id}
              className={`skin-card ${active ? "on" : ""}`}
              title={skin.description}
            >
              <div className="skin-card-head">
                <span className="skin-card-name">{skin.name}</span>
                {active && <span className="skin-card-active-tag">active</span>}
              </div>
              <div className="skin-card-hint">{skinTagline(skin)}</div>
              <div className="skin-card-desc">{skin.description}</div>
              <div className="skin-card-actions">
                <button
                  type="button"
                  className="settings-toggle skin-apply-btn"
                  disabled={active}
                  title={active ? "Already applied." : `Apply ${skin.name}.`}
                  onClick={() => applySkin(skin)}
                >
                  {active ? "applied" : "apply"}
                </button>
                <button
                  type="button"
                  className="settings-toggle skin-preview-btn"
                  onClick={() => setPreview(skin)}
                >
                  preview
                </button>
              </div>

              {active && (
                <>
                  <div className="skin-card-modules-label">parts</div>
                  <SkinModuleList variant="inline" />
                </>
              )}
            </div>
          );
        })}
      </div>

      <SkinMarketRow a={a} />

      {preview && (
        <SkinPreviewModal
          skin={preview}
          active={isSkinActive(a, activeCustom, preview)}
          onApply={() => {
            applySkin(preview);
            setPreview(null);
          }}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}
