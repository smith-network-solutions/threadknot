// Appearance + terminal preferences, persisted to localStorage and applied to
// the DOM. Changes broadcast on the window so live terminals can react without
// a reload. Mirrors the lightweight localStorage pattern used elsewhere
// (notify prefs, MainSplit layout).

import { ensureFontLoaded } from "./fonts";
import { traceMark } from "./renderTrace";

/** The theme palettes. "dark" (original) and "light" are unchanged; the four
 *  extra ids are curated data-theme blocks in styles.css. "light" and "solar"
 *  are the light-family palettes (pale backgrounds), the rest are dark. */
export type Theme = "dark" | "midnight" | "slate" | "carbon" | "light" | "solar" | "arcade";
export type TermCursorStyle = "block" | "bar" | "underline";

/** The 10 neutral palette CSS vars a custom theme may override, WITHOUT the
 *  leading "--". These match the vars each data-theme block declares in
 *  styles.css and the keys of CustomTheme.colors. Exported so the Settings UI
 *  can build its swatch editor from one list rather than a hardcoded array. */
export const NEUTRAL_VAR_KEYS = [
  "bg",
  "bg-raise",
  "panel",
  "panel-2",
  "panel-3",
  "line",
  "line-2",
  "text",
  "dim",
  "faint",
] as const;
export type NeutralVarKey = (typeof NEUTRAL_VAR_KEYS)[number];
const NEUTRAL_KEY_SET: ReadonlySet<string> = new Set(NEUTRAL_VAR_KEYS);

/** Wallpaper dim overlay is clamped to this band (0 = image at full strength,
 *  0.9 = almost black). Kept modest so the image can never fully hide content. */
export const BG_DIM_MIN = 0;
export const BG_DIM_MAX = 0.9;

/** Wallpaper zoom band: 1 = plain cover (no magnification), 3 = 3x. */
export const BG_ZOOM_MIN = 1;
export const BG_ZOOM_MAX = 3;
/** Stored pan range, a percentage of the available overflow in each axis:
 *  -100 = pushed hard to one edge, 0 = centered, 100 = the other edge. The
 *  actual CSS translate is derived from this by bgTranslatePct so the image
 *  edges can never enter the viewport (see the math there). */
export const BG_PAN_MIN = -100;
export const BG_PAN_MAX = 100;

/** A user-crafted theme record. STRUCTURALLY IDENTICAL to the backend's
 *  CustomTheme (theme.list / theme.save / theme.remove in protocol.ts);
 *  declared locally because appearance.ts must not import from protocol.ts.
 *  NOTE for the UI agent: unify this and the protocol type into one shared
 *  declaration once both landings merge (and reconcile createdAt/updatedAt if
 *  the backend serialises them as strings; nothing here reads those fields). */
export interface CustomTheme {
  id: string;
  name: string;
  /** A THEMES preset id used as the base palette + light/dark family. */
  base: string;
  /** An ACCENTS id or a "#rrggbb" hex; drives the --brass trio. */
  accent: string;
  /** Neutral var overrides keyed by NEUTRAL_VAR_KEYS (no leading "--"). */
  colors?: Record<string, string>;
  /** A data URL for the wallpaper, or undefined for none. */
  backgroundImage?: string;
  /** Wallpaper dim overlay strength, 0..0.9. */
  backgroundDim?: number;
  /** Wallpaper zoom, 1..3 (1 = plain cover). Absent = 1. */
  backgroundZoom?: number;
  /** Horizontal pan, -100..100 (0 = centered); percent of the available
   *  overflow in that axis. Absent = 0. */
  backgroundX?: number;
  /** Vertical pan, -100..100 (0 = centered); percent of the available overflow
   *  in that axis. Absent = 0. */
  backgroundY?: number;
  // Widened to `string | number` so the protocol.ts CustomTheme (which
  // serialises these as ISO strings) is structurally assignable here and the
  // Settings UI can pass a store record straight into reapplyWithCustomTheme.
  // Nothing in this module reads either field.
  createdAt: string | number;
  updatedAt: string | number;
}

export interface Appearance {
  theme: Theme;
  /** Accent: either a preset id from ACCENTS ("brass", "ember", ...) or a
   *  custom "#rrggbb" hex. Drives the --brass trio (see applyAppearance). */
  accent: string;
  /** UI font id from UI_FONTS; drives --font-ui. */
  fontUi: string;
  /** Monospace font id from MONO_FONTS; drives --font-mono. */
  fontMono: string;
  /** Conversation zoom multiplier: scales the thread's MESSAGE FEED only (see
   *  .feed-inner in styles.css). The thread header, composer, terminal,
   *  workspace panes, sidebar, conn banner and toasts always render at 100%,
   *  so nothing can be zoomed off-screen and the value needs no dynamic cap:
   *  ZOOM_MIN..ZOOM_MAX is the whole story. */
  uiZoom: number;
  /** The id of the active custom theme, or null when using the THEMES/ACCENTS
   *  presets as before. The RECORD itself lives in server state (appearance.ts
   *  has no store access); the UI passes it to applyAppearance /
   *  reapplyWithCustomTheme. This id only survives reloads so the UI knows
   *  which stored theme to re-apply. */
  customThemeId: string | null;
}

export interface TermPrefs {
  fontSize: number;
  cursorStyle: TermCursorStyle;
  cursorBlink: boolean;
  scrollback: number;
  /** Monospace font id from MONO_FONTS; wired into xterm's fontFamily. */
  fontFamily: string;
}

/** How wide the composer box is allowed to grow inside its pane. "cozy" is the
 *  900px cap Threadknot has always had, "wide" gives long prompts more room, and
 *  "full" lets it span the pane edge to edge. */
export type ComposerWidth = "cozy" | "wide" | "full";
/** Vertical breathing room in the composer card. "comfortable" is the shipped
 *  padding; "compact" tightens it so the message feed keeps more height. */
export type ComposerDensity = "comfortable" | "compact";

export const COMPOSER_WIDTHS: readonly ComposerWidth[] = ["cozy", "wide", "full"];
export const COMPOSER_DENSITIES: readonly ComposerDensity[] = ["comfortable", "compact"];

/** The zoom scales the message feed only, so the composer renders at true screen
 *  size; these are the knobs that size it instead. */
export interface ComposerPrefs {
  width: ComposerWidth;
  /** Composer textarea font size in px, CFONT_MIN..CFONT_MAX. */
  fontSize: number;
  density: ComposerDensity;
}

/** Swatch metadata for the Settings UI. preview colors are duplicated here as
 *  plain data so the picker can render chips without reading the stylesheet. */
export interface ThemeInfo {
  id: Theme;
  label: string;
  dark: boolean;
  preview: { bg: string; panel: string; accentDefault: string };
}

export const THEMES: readonly ThemeInfo[] = [
  { id: "dark", label: "Dark", dark: true, preview: { bg: "#0b0d12", panel: "#10141d", accentDefault: "#d9a35c" } },
  { id: "midnight", label: "Midnight", dark: true, preview: { bg: "#080b17", panel: "#0d1426", accentDefault: "#d9a35c" } },
  { id: "slate", label: "Slate", dark: true, preview: { bg: "#151b26", panel: "#1b2331", accentDefault: "#d9a35c" } },
  { id: "carbon", label: "Carbon", dark: true, preview: { bg: "#050506", panel: "#101013", accentDefault: "#d9a35c" } },
  { id: "light", label: "Light", dark: false, preview: { bg: "#f3f5f9", panel: "#ffffff", accentDefault: "#b07322" } },
  { id: "solar", label: "Solar", dark: false, preview: { bg: "#f4efe4", panel: "#fffdf7", accentDefault: "#b07322" } },
  { id: "arcade", label: "Retro-Tech", dark: true, preview: { bg: "#04030c", panel: "#0b0820", accentDefault: "#ff3d8a" } },
];

/** The bg color each palette lands on, for the browser/PWA theme-color meta. */
const THEME_BG: Record<Theme, string> = {
  dark: "#0b0d12",
  midnight: "#080b17",
  slate: "#151b26",
  carbon: "#050506",
  light: "#f3f5f9",
  solar: "#f4efe4",
  arcade: "#04030c",
};

const LIGHT_FAMILY: ReadonlySet<Theme> = new Set<Theme>(["light", "solar"]);

/** True for the pale-background palettes (light, solar). Callers that only knew
 *  about "light" (e.g. the terminal palette) use this to stay correct. */
export function isLightTheme(theme: Theme): boolean {
  return LIGHT_FAMILY.has(theme);
}

/** Accent presets. base is the mid-tone hex the --brass trio is derived from. */
export interface AccentPreset {
  id: string;
  label: string;
  base: string;
}

export const ACCENTS: readonly AccentPreset[] = [
  { id: "brass", label: "Brass", base: "#d9a35c" },
  { id: "ember", label: "Ember", base: "#e0705f" },
  { id: "coral", label: "Coral", base: "#e8896a" },
  { id: "teal", label: "Teal", base: "#5fc6b0" },
  { id: "ocean", label: "Ocean", base: "#6aa6e8" },
  { id: "violet", label: "Violet", base: "#a58fe0" },
  { id: "emerald", label: "Emerald", base: "#6fca8f" },
  { id: "rose", label: "Rose", base: "#dd7fa8" },
  { id: "steel", label: "Steel", base: "#9aa7b8" },
];

/** Font choices. stack is the CSS font-family list pushed onto a --font-* var;
 *  the first entry that resolves on the machine wins, hence the fallbacks.
 *  `google` is the Google Fonts family query (e.g. "Space+Grotesk:wght@400;600")
 *  for entries that need webfont loading; it is fed to ensureFontLoaded on
 *  demand (see lib/fonts.ts). System-stack entries — and the default Archivo /
 *  JetBrains Mono, which already ship via the styles.css @import — omit it. */
export interface FontOption {
  id: string;
  label: string;
  stack: string;
  /** Google Fonts family query, loaded lazily. Omit for system/@import fonts. */
  google?: string;
}

// UI families request 400;600 (600 covers the semibold weights), except the
// handful of static families whose nearest heavier cut is 700. archivo,
// system, georgia and verdana carry no `google` (default @import / OS-native).
export const UI_FONTS: readonly FontOption[] = [
  { id: "archivo", label: "Archivo", stack: '"Archivo", "Avenir Next", "Segoe UI", system-ui, sans-serif' },
  { id: "inter", label: "Inter", stack: '"Inter", "Segoe UI", sans-serif', google: "Inter:wght@400;600" },
  { id: "system", label: "System", stack: '"Segoe UI", system-ui, sans-serif' },
  { id: "atkinson", label: "Atkinson Hyperlegible", stack: '"Atkinson Hyperlegible", "Segoe UI", sans-serif', google: "Atkinson+Hyperlegible:wght@400;700" },
  { id: "poppins", label: "Poppins", stack: '"Poppins", "Segoe UI", sans-serif', google: "Poppins:wght@400;600" },
  { id: "montserrat", label: "Montserrat", stack: '"Montserrat", "Segoe UI", sans-serif', google: "Montserrat:wght@400;600" },
  { id: "roboto", label: "Roboto", stack: '"Roboto", "Segoe UI", sans-serif', google: "Roboto:wght@400;500" },
  { id: "opensans", label: "Open Sans", stack: '"Open Sans", "Segoe UI", sans-serif', google: "Open+Sans:wght@400;600" },
  { id: "lato", label: "Lato", stack: '"Lato", "Segoe UI", sans-serif', google: "Lato:wght@400;700" },
  { id: "nunito", label: "Nunito", stack: '"Nunito", "Segoe UI", sans-serif', google: "Nunito:wght@400;600" },
  { id: "raleway", label: "Raleway", stack: '"Raleway", "Segoe UI", sans-serif', google: "Raleway:wght@400;600" },
  { id: "worksans", label: "Work Sans", stack: '"Work Sans", "Segoe UI", sans-serif', google: "Work+Sans:wght@400;600" },
  { id: "dmsans", label: "DM Sans", stack: '"DM Sans", "Segoe UI", sans-serif', google: "DM+Sans:wght@400;600" },
  { id: "manrope", label: "Manrope", stack: '"Manrope", "Segoe UI", sans-serif', google: "Manrope:wght@400;600" },
  { id: "spacegrotesk", label: "Space Grotesk", stack: '"Space Grotesk", "Segoe UI", sans-serif', google: "Space+Grotesk:wght@400;600" },
  { id: "outfit", label: "Outfit", stack: '"Outfit", "Segoe UI", sans-serif', google: "Outfit:wght@400;600" },
  { id: "sora", label: "Sora", stack: '"Sora", "Segoe UI", sans-serif', google: "Sora:wght@400;600" },
  { id: "karla", label: "Karla", stack: '"Karla", "Segoe UI", sans-serif', google: "Karla:wght@400;600" },
  { id: "rubik", label: "Rubik", stack: '"Rubik", "Segoe UI", sans-serif', google: "Rubik:wght@400;600" },
  { id: "quicksand", label: "Quicksand", stack: '"Quicksand", "Segoe UI", sans-serif', google: "Quicksand:wght@400;600" },
  { id: "josefin", label: "Josefin Sans", stack: '"Josefin Sans", "Segoe UI", sans-serif', google: "Josefin+Sans:wght@400;600" },
  { id: "merriweather", label: "Merriweather", stack: '"Merriweather", Georgia, serif', google: "Merriweather:wght@400;700" },
  { id: "playfair", label: "Playfair Display", stack: '"Playfair Display", Georgia, serif', google: "Playfair+Display:wght@400;600" },
  { id: "lora", label: "Lora", stack: '"Lora", Georgia, serif', google: "Lora:wght@400;600" },
  { id: "crimson", label: "Crimson Pro", stack: '"Crimson Pro", Georgia, serif', google: "Crimson+Pro:wght@400;600" },
  { id: "georgia", label: "Georgia", stack: 'Georgia, "Times New Roman", serif' },
  { id: "verdana", label: "Verdana", stack: "Verdana, Geneva, sans-serif" },
];

// Mono families request 400;700 uniformly (every listed family ships both cuts).
// jetbrains ships via the @import; cascadia, consolas and courier are OS-native.
export const MONO_FONTS: readonly FontOption[] = [
  { id: "jetbrains", label: "JetBrains Mono", stack: '"JetBrains Mono", "Fira Code", ui-monospace, "SF Mono", monospace' },
  { id: "cascadia", label: "Cascadia Code", stack: '"Cascadia Code", "Cascadia Mono", Consolas, monospace' },
  { id: "fira", label: "Fira Code", stack: '"Fira Code", monospace', google: "Fira+Code:wght@400;700" },
  { id: "consolas", label: "Consolas", stack: 'Consolas, "Courier New", monospace' },
  { id: "sourcecode", label: "Source Code Pro", stack: '"Source Code Pro", monospace', google: "Source+Code+Pro:wght@400;700" },
  { id: "courier", label: "Courier New", stack: '"Courier New", monospace' },
  { id: "ibmplex", label: "IBM Plex Mono", stack: '"IBM Plex Mono", ui-monospace, monospace', google: "IBM+Plex+Mono:wght@400;700" },
  { id: "spacemono", label: "Space Mono", stack: '"Space Mono", ui-monospace, monospace', google: "Space+Mono:wght@400;700" },
  { id: "inconsolata", label: "Inconsolata", stack: '"Inconsolata", ui-monospace, monospace', google: "Inconsolata:wght@400;700" },
  { id: "ubuntumono", label: "Ubuntu Mono", stack: '"Ubuntu Mono", ui-monospace, monospace', google: "Ubuntu+Mono:wght@400;700" },
  { id: "robotomono", label: "Roboto Mono", stack: '"Roboto Mono", ui-monospace, monospace', google: "Roboto+Mono:wght@400;700" },
  { id: "victormono", label: "Victor Mono", stack: '"Victor Mono", ui-monospace, monospace', google: "Victor+Mono:wght@400;700" },
  { id: "anonymous", label: "Anonymous Pro", stack: '"Anonymous Pro", ui-monospace, monospace', google: "Anonymous+Pro:wght@400;700" },
  { id: "redhatmono", label: "Red Hat Mono", stack: '"Red Hat Mono", ui-monospace, monospace', google: "Red+Hat+Mono:wght@400;700" },
];

/** Resolve a font id to its catalog entry, falling back to the list's first
 *  entry (the default) when the id is unknown. Exported so the FontPicker and
 *  applyAppearance can reach an entry's `google` field for on-demand loading. */
export function uiFontEntry(id: string): FontOption {
  return UI_FONTS.find((f) => f.id === id) ?? UI_FONTS[0];
}
export function monoFontEntry(id: string): FontOption {
  return MONO_FONTS.find((f) => f.id === id) ?? MONO_FONTS[0];
}

/** Resolve a font id to its CSS stack, falling back to the list's first entry
 *  (the default) when the id is unknown. */
export function uiFontStack(id: string): string {
  return uiFontEntry(id).stack;
}
export function monoFontStack(id: string): string {
  return monoFontEntry(id).stack;
}

/** How the sidebar presents the project layer.
 *  - `sections`: every project expanded at once (the original layout).
 *  - `accordion`: opening one project closes the others — all the headers
 *    stay visible, but only one list is ever on screen.
 *  - `picker`: one project at a time chosen from a dropdown, no headers.
 *  - `rail`: a chat-app-style icon rail - a column of project avatars pinned down the
 *    left edge, the selected project's chats filling the rest. Switching is
 *    one tap with no menu, and every project stays visible (and badgeable)
 *    the whole time. */
export type ProjectLayout = "sections" | "accordion" | "picker" | "rail";

export const PROJECT_LAYOUTS: readonly ProjectLayout[] = [
  "sections",
  "accordion",
  "picker",
  "rail",
];

export interface SidebarPrefs {
  /** Days of silence after which a chat settles itself into the shelf.
   *  null = never auto-settle; only explicit settles park a chat. */
  autoSettleDays: number | null;
  projectLayout: ProjectLayout;
}

/** The parts a skin is cut into. Each id gates one block of skin CSS through
 *  `:root[data-theme="..."]:not([data-skin-off~="<id>"])`, so a user can keep
 *  the pieces they like (the cabinet chrome) and drop the ones they don't (the
 *  themed sidebar cards) instead of taking the whole theme or none of it. */
export type SkinModule = "usage" | "parley" | "cards" | "terminal" | "workspace";

/** Which skin modules are turned OFF. Stored as the off-list rather than the
 *  on-list so a skin that grows a new module later ships it enabled, and so an
 *  untouched install persists nothing at all. */
export interface SkinPrefs {
  off: SkinModule[];
}

/** Canonical module order: the stored list and the data attribute are always
 *  emitted in it, so the same set of disabled modules always serialises the
 *  same way no matter what order the toggles were flipped in. */
const SKIN_MODULE_IDS: readonly SkinModule[] = [
  "usage",
  "parley",
  "cards",
  "terminal",
  "workspace",
];

const A_KEY = "threadknot.appearance";
const T_KEY = "threadknot.termprefs";
const S_KEY = "threadknot.sidebarprefs";
const C_KEY = "threadknot.composerprefs";
const K_KEY = "threadknot.skinprefs";

export const ZOOM_MIN = 0.8;
/** Only the message feed scales, so the range can be generous: nothing else
 *  moves and the feed reflows into whatever width the scrollport has. */
export const ZOOM_MAX = 2;
export const ZOOM_STEP = 0.05;
export const TFONT_MIN = 9;
export const TFONT_MAX = 24;
export const SCROLLBACK_MIN = 1000;
export const SCROLLBACK_MAX = 50000;

/** Composer text band. The floor stays readable, the ceiling stops the box from
 *  eating the feed (the 220px autogrow cap is unchanged either way). */
export const CFONT_MIN = 12;
export const CFONT_MAX = 18;

/** What each width preset writes into --composer-max-w. "cozy" is the exact
 *  900px .composer has always carried, so the default preserves today's layout;
 *  "full" is a no-op cap against the pane's own width. */
const COMPOSER_MAX_W: Record<ComposerWidth, string> = {
  cozy: "900px",
  wide: "1040px",
  full: "100%",
};

export const AUTOSETTLE_MIN = 1;
export const AUTOSETTLE_MAX = 90;
/** What the toggle turns auto-settle back ON to. */
export const AUTOSETTLE_DEFAULT = 3;

export const APPEARANCE_EVENT = "threadknot:appearance";
export const TERMPREFS_EVENT = "threadknot:termprefs";
export const SIDEBARPREFS_EVENT = "threadknot:sidebarprefs";
export const COMPOSERPREFS_EVENT = "threadknot:composerprefs";
/** Fired when a skin module is toggled. detail: the new SkinPrefs. CSS reacts
 *  on its own through the data attribute; this is for the consumers that paint
 *  outside CSS's reach (the xterm buffer palette). */
export const SKINPREFS_EVENT = "threadknot:skinprefs";
/** Fired whenever the APPLIED palette family flips (dark <-> light) because a
 *  preset, a custom theme's base, or a live studio preview changed it. detail:
 *  the resolved "dark" | "light". Consumers that must track the real family
 *  (e.g. the terminal palette) listen here rather than reading Appearance.theme,
 *  which ignores an active custom theme's base. */
export const THEME_FAMILY_EVENT = "threadknot:themefamily";
export type ThemeFamily = "dark" | "light";
/** Fired when the APPLIED zoom changes for a reason other than the stored
 *  preference moving (APPEARANCE_EVENT already covers that). detail: applied
 *  zoom. Feed-only zoom has no dynamic cap, so nothing fires this today; the
 *  event is kept so listeners stay wired if a cap ever comes back. */
export const ZOOM_APPLIED_EVENT = "threadknot:zoomapplied";

const A_DEFAULT: Appearance = {
  theme: "dark",
  accent: "brass",
  fontUi: "archivo",
  fontMono: "jetbrains",
  uiZoom: 1,
  customThemeId: null,
};
const S_DEFAULT: SidebarPrefs = {
  autoSettleDays: AUTOSETTLE_DEFAULT,
  // The rail is the layout the app is built around now: every project stays on
  // screen and badgeable, switching costs one tap, and the chat list gets the
  // full sidebar width instead of sharing it with a stack of headers. Only new
  // installs land here — anyone who has already touched a sidebar preference
  // has `projectLayout` written to localStorage and keeps whatever they chose.
  projectLayout: "rail",
};
const T_DEFAULT: TermPrefs = {
  fontSize: 13,
  cursorStyle: "bar",
  cursorBlink: true,
  scrollback: 10000,
  fontFamily: "jetbrains",
};
/** Nothing disabled: pick a skin and you get all of it until you say otherwise. */
const K_DEFAULT: SkinPrefs = { off: [] };
// Exactly what styles.css shipped: .composer's 900px cap, the textarea's 14px,
// and the card's original paddings. Nobody's composer moves on update.
const C_DEFAULT: ComposerPrefs = {
  width: "cozy",
  fontSize: 14,
  density: "comfortable",
};

export function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

// ---- wallpaper zoom + placement math ---------------------------------------
// The wallpaper renders on a viewport-sized, position:fixed body::before with
// background-size: cover, then gains
//   transform: scale(z) translate(tx, ty)   (transform-origin: center)
// where tx/ty are PERCENTAGES OF THE ELEMENT. Because the transform is
// scale-then-translate, a tx% offset lands z*tx% of the element away from
// centre in viewport space; keeping the scaled (z*W by z*H) element covering the
// W by H viewport bounds |z*tx%| to (z-1)/2, i.e. |tx| <= (z-1)/(2z). We store
// placement as -100..100 (percent of that available slack) and convert to the
// real translate percentage here, so the CSS stays dumb: it only ever plugs in
// --app-bg-zoom / --app-bg-tx / --app-bg-ty. Exported for the studio UI agent so
// its live preview reproduces the exact same geometry.

/** Max |translate|, as a percentage of the element, that keeps a `cover` image
 *  fully covering the viewport at the given zoom: ((z - 1) / (2 * z)) * 100.
 *  Zoom <= 1 has no slack (cover already fits), so this is 0. */
export function bgMaxShiftPct(zoom: number): number {
  const z = clamp(Number.isFinite(zoom) ? zoom : 1, BG_ZOOM_MIN, BG_ZOOM_MAX);
  if (z <= 1) return 0;
  return ((z - 1) / (2 * z)) * 100;
}

/** Convert a stored pan value (-100..100, percent of the available overflow in
 *  one axis) into the actual CSS translate percentage of the element for the
 *  given zoom: (stored / 100) * bgMaxShiftPct(zoom). The result is clamped by
 *  construction so image edges never enter the viewport. */
export function bgTranslatePct(zoom: number, stored: number): number {
  const s = clamp(Number.isFinite(stored) ? stored : 0, BG_PAN_MIN, BG_PAN_MAX);
  return (s / 100) * bgMaxShiftPct(zoom);
}

// ---- accent color math ----------------------------------------------------
// Small self-contained hex<->hsl helpers so the accent trio can be derived at
// apply time. Kept here (not in ColorPicker) so appearance.ts has no UI dep.

export interface Rgb {
  r: number;
  g: number;
  b: number;
}

export function parseHex(hex: string): Rgb | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

export function toHex({ r, g, b }: Rgb): string {
  const h = (v: number) => Math.round(clamp(v, 0, 255)).toString(16).padStart(2, "0");
  return `#${h(r)}${h(g)}${h(b)}`;
}

export function rgbToHsl({ r, g, b }: Rgb): { h: number; s: number; l: number } {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const d = max - min;
  const l = (max + min) / 2;
  let h = 0;
  let s = 0;
  if (d !== 0) {
    s = d / (1 - Math.abs(2 * l - 1));
    if (max === rn) h = ((gn - bn) / d) % 6;
    else if (max === gn) h = (bn - rn) / d + 2;
    else h = (rn - gn) / d + 4;
    h *= 60;
    if (h < 0) h += 360;
  }
  return { h, s, l };
}

export function hslToRgb(h: number, s: number, l: number): Rgb {
  const c = (1 - Math.abs(2 * l - 1)) * s;
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
  const m = l - c / 2;
  return { r: (rn + m) * 255, g: (gn + m) * 255, b: (bn + m) * 255 };
}

/** Shift a hex color's lightness by deltaL (in 0..1 units; negative darkens). */
export function shiftL(hex: string, deltaL: number): string {
  const rgb = parseHex(hex);
  if (!rgb) return hex;
  const { h, s, l } = rgbToHsl(rgb);
  return toHex(hslToRgb(h, s, clamp(l + deltaL, 0, 1)));
}

/** The base color the --brass trio derives from: a preset id maps to its base
 *  hex, a valid "#rrggbb" is used as-is, anything else is the brass default. */
function accentBaseHex(accent: string): string {
  const preset = ACCENTS.find((a) => a.id === accent);
  if (preset) return preset.base;
  return parseHex(accent) ? accent.toLowerCase() : ACCENTS[0].base;
}

/** Family-usable lightness bands for the accent base. An extreme custom hex
 *  (#000000 on a dark theme, #ffffff on a light one) would otherwise derive a
 *  trio that is invisible against the surface, so the base's lightness is
 *  clamped into these ranges (near the shipped brass values) before the trio is
 *  built. Hue and saturation are preserved, so the picked colour is honoured. */
const ACCENT_L_DARK: readonly [number, number] = [0.45, 0.8];
const ACCENT_L_LIGHT: readonly [number, number] = [0.25, 0.55];

function clampAccentL(hex: string, lightFamily: boolean): string {
  const rgb = parseHex(hex);
  if (!rgb) return hex;
  const { h, s, l } = rgbToHsl(rgb);
  const [lo, hi] = lightFamily ? ACCENT_L_LIGHT : ACCENT_L_DARK;
  return toHex(hslToRgb(h, s, clamp(l, lo, hi)));
}

/** Compute the { brass, brassHi, brassDim } strings for an accent on a given
 *  theme family. The default "brass" is special-cased to the exact hand-picked
 *  values so the shipped look never drifts; every other accent is derived:
 *  -hi lightens ~18% on dark palettes, darkens ~25% on light ones (and the
 *  base darkens a touch there for contrast on pale surfaces), and -dim is the
 *  base at 14% alpha. */
function brassTrio(
  accent: string,
  lightFamily: boolean,
): { brass: string; hi: string; dim: string } {
  if (accent === "brass") {
    return lightFamily
      ? { brass: "#b07322", hi: "#855309", dim: "rgba(176, 115, 34, 0.14)" }
      : { brass: "#d9a35c", hi: "#f2c98a", dim: "rgba(217, 163, 92, 0.14)" };
  }
  // Clamp the base into the family's usable lightness band first, so an extreme
  // custom hex can't derive an invisible/unreadable trio (hue/sat preserved).
  const base0 = clampAccentL(accentBaseHex(accent), lightFamily);
  const brass = lightFamily ? shiftL(base0, -0.12) : base0;
  const hi = lightFamily ? shiftL(base0, -0.25) : shiftL(base0, 0.18);
  const rgb = parseHex(brass) ?? { r: 217, g: 163, b: 92 };
  const dim = `rgba(${Math.round(rgb.r)}, ${Math.round(rgb.g)}, ${Math.round(rgb.b)}, 0.14)`;
  return { brass, hi, dim };
}

/** Unknown/hand-edited stored values fall back rather than breaking render. */
function normalizeTheme(value: unknown): Theme {
  return THEMES.some((t) => t.id === value) ? (value as Theme) : "dark";
}

function normalizeAccent(value: unknown): string {
  if (typeof value !== "string") return "brass";
  if (ACCENTS.some((a) => a.id === value)) return value;
  return parseHex(value) ? value.toLowerCase() : "brass";
}

function normalizeFontId(value: unknown, list: readonly FontOption[]): string {
  return list.some((f) => f.id === value) ? (value as string) : list[0].id;
}

/** A non-empty string is a real custom-theme id; anything else means "presets".
 *  Kept tolerant so a hand-edited or older stored value never breaks render. */
function normalizeCustomThemeId(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function read<T extends object>(key: string, def: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (raw) return { ...def, ...(JSON.parse(raw) as Partial<T>) };
  } catch {
    /* fall through to default */
  }
  return { ...def };
}

export function getAppearance(): Appearance {
  const a = read(A_KEY, A_DEFAULT);
  return {
    theme: normalizeTheme(a.theme),
    accent: normalizeAccent(a.accent),
    fontUi: normalizeFontId(a.fontUi, UI_FONTS),
    fontMono: normalizeFontId(a.fontMono, MONO_FONTS),
    uiZoom: clamp(a.uiZoom, ZOOM_MIN, ZOOM_MAX),
    customThemeId: normalizeCustomThemeId(a.customThemeId),
  };
}

/** An unknown layout (older build, hand-edited storage) falls back rather
 *  than rendering nothing. */
function readProjectLayout(value: unknown): ProjectLayout {
  return PROJECT_LAYOUTS.includes(value as ProjectLayout)
    ? (value as ProjectLayout)
    : S_DEFAULT.projectLayout;
}

/** null (never auto-settle) is a real stored value, so it has to survive the
 *  round trip; anything else non-numeric falls back to the default rather
 *  than silently disabling auto-settle. */
export function getSidebarPrefs(): SidebarPrefs {
  const s = read(S_KEY, S_DEFAULT);
  const projectLayout = readProjectLayout(s.projectLayout);
  if (s.autoSettleDays === null) return { autoSettleDays: null, projectLayout };
  const days = Number(s.autoSettleDays);
  if (!Number.isFinite(days)) {
    return { autoSettleDays: S_DEFAULT.autoSettleDays, projectLayout };
  }
  return {
    autoSettleDays: clamp(Math.round(days), AUTOSETTLE_MIN, AUTOSETTLE_MAX),
    projectLayout,
  };
}

export function setSidebarPrefs(next: SidebarPrefs): void {
  const projectLayout = readProjectLayout(next.projectLayout);
  const clamped: SidebarPrefs =
    next.autoSettleDays === null
      ? { autoSettleDays: null, projectLayout }
      : {
          autoSettleDays: clamp(
            Math.round(next.autoSettleDays),
            AUTOSETTLE_MIN,
            AUTOSETTLE_MAX,
          ),
          projectLayout,
        };
  localStorage.setItem(S_KEY, JSON.stringify(clamped));
  window.dispatchEvent(new CustomEvent<SidebarPrefs>(SIDEBARPREFS_EVENT, { detail: clamped }));
}

export function getTermPrefs(): TermPrefs {
  const t = read(T_KEY, T_DEFAULT);
  return {
    fontSize: clamp(Math.round(t.fontSize), TFONT_MIN, TFONT_MAX),
    cursorStyle:
      t.cursorStyle === "block" || t.cursorStyle === "underline" ? t.cursorStyle : "bar",
    cursorBlink: !!t.cursorBlink,
    scrollback: clamp(Math.round(t.scrollback), SCROLLBACK_MIN, SCROLLBACK_MAX),
    fontFamily: normalizeFontId(t.fontFamily, MONO_FONTS),
  };
}

/** True when the terminal preferences are untouched: no stored record at all,
 *  or one that normalizes straight back to the shipped defaults. A skin that
 *  repaints the terminal (the arcade phosphor look) checks this first, because
 *  a hand-picked cursor or font is a deliberate choice a skin must not
 *  silently overrule. */
export function termPrefsAreDefault(): boolean {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(T_KEY);
  } catch {
    // No storage to disagree with the defaults.
    return true;
  }
  if (!raw) return true;
  // Compare through getTermPrefs so a stored record that only differs in ways
  // normalization erases (an unknown font id, an out-of-band size) still counts
  // as default.
  const t = getTermPrefs();
  return (Object.keys(T_DEFAULT) as (keyof TermPrefs)[]).every((k) => t[k] === T_DEFAULT[k]);
}

/** Drop unknown ids (older/newer build, hand-edited storage), dedupe, and
 *  return them in the canonical order. */
function normalizeSkinOff(value: unknown): SkinModule[] {
  if (!Array.isArray(value)) return [];
  const off = new Set(value as SkinModule[]);
  return SKIN_MODULE_IDS.filter((id) => off.has(id));
}

export function getSkinPrefs(): SkinPrefs {
  return { off: normalizeSkinOff(read(K_KEY, K_DEFAULT).off) };
}

/** Mirror the off-list onto :root as a space-separated token list, REMOVING the
 *  attribute when nothing is off so the CSS gates can be a plain
 *  `:not([data-skin-off~="x"])` with no empty-string special case. */
function applySkinPrefs(p: SkinPrefs, root: HTMLElement = document.documentElement): void {
  if (p.off.length === 0) delete root.dataset.skinOff;
  else root.dataset.skinOff = p.off.join(" ");
}

/** Resolve the phosphor terminal's on/off state ONCE and mirror it onto :root as
 *  data-phosphor. It is three conditions at a time (the arcade palette applied,
 *  its terminal module left on, and untouched terminal preferences), and two
 *  halves of the app have to agree on the answer: the scanline/glow CSS and the
 *  xterm buffer palette. Each computing its own is how a green-scanlined normal
 *  terminal happens, so both read this attribute instead. Must run after the
 *  data-theme and data-skin-off writes it depends on. */
function applyPhosphorFlag(root: HTMLElement = document.documentElement): void {
  const off = (root.dataset.skinOff ?? "").split(/\s+/);
  const on =
    root.dataset.theme === "arcade" && !off.includes("terminal") && termPrefsAreDefault();
  if (on) root.dataset.phosphor = "on";
  else delete root.dataset.phosphor;
}

export function setSkinPrefs(next: SkinPrefs): void {
  const clean: SkinPrefs = { off: normalizeSkinOff(next.off) };
  localStorage.setItem(K_KEY, JSON.stringify(clean));
  // Written straight to the DOM as well: a toggle should land instantly without
  // dragging a whole applyAppearance pass (fonts, wallpaper, meta color) with it.
  applySkinPrefs(clean);
  // Before the event, so a listener that re-reads the DOM sees the new answer.
  applyPhosphorFlag();
  window.dispatchEvent(new CustomEvent<SkinPrefs>(SKINPREFS_EVENT, { detail: clean }));
}

/** Unknown/hand-edited stored values fall back rather than breaking layout. */
function normalizeComposerWidth(value: unknown): ComposerWidth {
  return COMPOSER_WIDTHS.includes(value as ComposerWidth)
    ? (value as ComposerWidth)
    : C_DEFAULT.width;
}

function normalizeComposerDensity(value: unknown): ComposerDensity {
  return COMPOSER_DENSITIES.includes(value as ComposerDensity)
    ? (value as ComposerDensity)
    : C_DEFAULT.density;
}

export function getComposerPrefs(): ComposerPrefs {
  const c = read(C_KEY, C_DEFAULT);
  const size = Number(c.fontSize);
  return {
    width: normalizeComposerWidth(c.width),
    fontSize: Number.isFinite(size)
      ? clamp(Math.round(size), CFONT_MIN, CFONT_MAX)
      : C_DEFAULT.fontSize,
    density: normalizeComposerDensity(c.density),
  };
}

/** Push the composer size knobs onto :root. The two measurements ride CSS vars
 *  (.composer / .composer-card textarea read them) and density rides a data
 *  attribute, so every composer instance follows without a React subscription.
 *  At the defaults this writes 900px / 14px / "comfortable", which is precisely
 *  what the stylesheet already hardcoded. */
export function applyComposerPrefs(c: ComposerPrefs = getComposerPrefs()): void {
  const root = document.documentElement;
  root.style.setProperty("--composer-max-w", COMPOSER_MAX_W[c.width]);
  root.style.setProperty("--composer-font", `${c.fontSize}px`);
  root.dataset.composerDensity = c.density;
}

export function setComposerPrefs(next: ComposerPrefs): void {
  const clamped: ComposerPrefs = {
    width: normalizeComposerWidth(next.width),
    fontSize: clamp(Math.round(next.fontSize), CFONT_MIN, CFONT_MAX),
    density: normalizeComposerDensity(next.density),
  };
  localStorage.setItem(C_KEY, JSON.stringify(clamped));
  applyComposerPrefs(clamped);
  window.dispatchEvent(
    new CustomEvent<ComposerPrefs>(COMPOSERPREFS_EVENT, { detail: clamped }),
  );
}

/** The zoom ceiling. This used to shrink with the work-area pane so the zoomed
 *  COMPOSER could never be pushed off-screen; the zoom now scales the message
 *  feed alone (the composer, header and every other pane stay at 1x), so there
 *  is nothing left to protect and the ceiling is simply ZOOM_MAX. Kept as a
 *  function because callers (hotwheel, the zoom chip) read it. */
export function getEffectiveMaxZoom(): number {
  return ZOOM_MAX;
}

/** The zoom actually applied to the feed. getAppearance already clamps the
 *  stored preference into ZOOM_MIN..ZOOM_MAX, so this is that value; kept as
 *  its own function so callers stay honest about applied vs. stored. */
export function getAppliedZoom(): number {
  return getAppearance().uiZoom;
}

/**
 * No-op, kept so callers do not break. Feed-only zoom needs no pane
 * measurement: the stored preference is applied verbatim regardless of how
 * small the work area gets.
 */
export function setZoomPaneSize(_width: number, _height: number): void {
  /* intentionally empty */
}

// ---- custom (user-crafted) theme application -------------------------------
// The active CustomTheme RECORD comes from server state, which appearance.ts
// cannot reach. It is tracked here as module state: whoever has the record
// (the UI, watching the store) hands it in via applyAppearance's second arg or
// reapplyWithCustomTheme; every other applyAppearance() call (preset edits,
// zoom/font tweaks) preserves it. null = presets only.

let activeCustomTheme: CustomTheme | null = null;
// The exact inline neutral vars the last custom theme wrote, so switching or
// clearing a theme never leaves a stale --panel / --text etc. behind.
let appliedCustomVars: string[] = [];
// The palette family actually on screen (a custom theme's base can flip this
// away from Appearance.theme). Seeded to the default palette's family; kept in
// step by applyAppearance, which fires THEME_FAMILY_EVENT on every change.
let appliedThemeFamily: ThemeFamily = LIGHT_FAMILY.has(A_DEFAULT.theme) ? "light" : "dark";

/** The palette family currently applied to the DOM: "light" for the pale
 *  palettes (including a custom theme whose base is light), else "dark". The
 *  terminal reads this instead of Appearance.theme so a light-based custom
 *  theme (and its live preview) renders a light terminal. */
export function getAppliedThemeFamily(): ThemeFamily {
  return appliedThemeFamily;
}

/** Set (or clear) the wallpaper vars + data-has-bg on :root. styles.css renders
 *  the image via body::before, scoped entirely under :root[data-has-bg]. */
function applyBackgroundImage(
  root: HTMLElement,
  image: string | undefined,
  dim: number | undefined,
  zoom: number | undefined,
  x: number | undefined,
  y: number | undefined,
): void {
  if (image) {
    // Quote the data URL: it contains commas that would otherwise break url().
    root.style.setProperty("--app-bg-image", `url("${image}")`);
    root.style.setProperty("--app-bg-dim", String(clamp(dim ?? 0, BG_DIM_MIN, BG_DIM_MAX)));
    // Zoom + placement: clamp zoom, then resolve X/Y into real translate
    // percentages here so the CSS only plugs the vars into its transform.
    const z = clamp(Number.isFinite(zoom) ? (zoom as number) : 1, BG_ZOOM_MIN, BG_ZOOM_MAX);
    root.style.setProperty("--app-bg-zoom", String(z));
    root.style.setProperty("--app-bg-tx", `${bgTranslatePct(z, x ?? 0)}%`);
    root.style.setProperty("--app-bg-ty", `${bgTranslatePct(z, y ?? 0)}%`);
    root.setAttribute("data-has-bg", "");
  } else {
    root.style.removeProperty("--app-bg-image");
    root.style.removeProperty("--app-bg-dim");
    root.style.removeProperty("--app-bg-zoom");
    root.style.removeProperty("--app-bg-tx");
    root.style.removeProperty("--app-bg-ty");
    root.removeAttribute("data-has-bg");
  }
}

/**
 * Apply a custom theme's NEUTRAL palette overrides + wallpaper, or clear them
 * when null. The base data-theme/family and the accent trio are set by
 * applyAppearance (from theme.base / theme.accent when a custom theme is
 * active), so this handles only the parts unique to a custom theme:
 *  - each `colors` entry becomes an inline `--<key>` on :root (winning over the
 *    data-theme block), validated against NEUTRAL_VAR_KEYS,
 *  - the background image (see applyBackgroundImage).
 * Every var it writes is tracked in appliedCustomVars and removed on the next
 * call, so no stale inline color survives a theme switch.
 */
export function applyCustomTheme(theme: CustomTheme | null): void {
  const root = document.documentElement;
  for (const name of appliedCustomVars) root.style.removeProperty(name);
  appliedCustomVars = [];
  if (!theme) {
    applyBackgroundImage(root, undefined, undefined, undefined, undefined, undefined);
    return;
  }
  if (theme.colors) {
    for (const [key, value] of Object.entries(theme.colors)) {
      if (!NEUTRAL_KEY_SET.has(key) || typeof value !== "string") continue;
      const name = `--${key}`;
      root.style.setProperty(name, value);
      appliedCustomVars.push(name);
    }
  }
  applyBackgroundImage(
    root,
    theme.backgroundImage,
    theme.backgroundDim,
    theme.backgroundZoom,
    theme.backgroundX,
    theme.backgroundY,
  );
}

/** Push theme + accent + fonts + zoom onto the root element so CSS
 *  picks them up. The palette lives in a data-theme block; the accent trio and
 *  the two font stacks are set inline (they override the stylesheet defaults).
 *
 *  When a CustomTheme is active it takes over the palette: data-theme/family
 *  follow theme.base, the accent trio derives from theme.accent through the
 *  same brassTrio math, and the neutral overrides + wallpaper are applied.
 *
 *  The second argument controls the active custom theme:
 *   - omitted        -> keep whatever theme is currently active (used by the
 *                       preset/zoom/font edits so they never drop it),
 *   - a CustomTheme  -> make it active,
 *   - null           -> clear it, returning to the presets.
 *  Fonts and zoom always come from the Appearance, never the custom theme. */
export function applyAppearance(
  a: Appearance = getAppearance(),
  customTheme?: CustomTheme | null,
): void {
  const root = document.documentElement;
  // Rewrites every root-level custom property: a full repaint, with no React
  // commit for the tracer to attribute it to unless it is marked here.
  traceMark("applyAppearance", a.theme);
  // Only touch the tracked theme when the caller actually passed the arg;
  // `undefined` means "leave as-is", `null` means "clear".
  if (arguments.length >= 2) activeCustomTheme = customTheme ?? null;
  const custom = activeCustomTheme;

  // A custom theme borrows its base preset's palette + light/dark family.
  const paletteTheme: Theme = custom ? normalizeTheme(custom.base) : a.theme;
  const lightFamily = LIGHT_FAMILY.has(paletteTheme);
  root.dataset.theme = paletteTheme;
  // Skin module toggles ride the same element, so a skin's CSS can gate each of
  // its parts independently of the palette it ships with.
  applySkinPrefs(getSkinPrefs(), root);
  // Both inputs it reads (data-theme, data-skin-off) have just landed.
  applyPhosphorFlag(root);
  // data-family groups the pale palettes so the light surface overrides in
  // styles.css cover both "light" and "solar" without duplicating every rule.
  const family: ThemeFamily = lightFamily ? "light" : "dark";
  root.dataset.family = family;
  // Announce a family flip so palette-family consumers (the terminal) can react
  // to a custom theme's base or a live studio preview — cases setAppearance's
  // APPEARANCE_EVENT alone never covers. Fired only on change, so the routine
  // applyAppearance() calls (zoom steps, font tweaks) stay quiet.
  if (family !== appliedThemeFamily) {
    appliedThemeFamily = family;
    window.dispatchEvent(new CustomEvent<ThemeFamily>(THEME_FAMILY_EVENT, { detail: family }));
  }
  // --ui-zoom drives the message feed's `zoom` (see .feed-inner). Clamped here
  // too so a live preview passing an out-of-band value cannot escape the band.
  root.style.setProperty("--ui-zoom", String(clamp(a.uiZoom, ZOOM_MIN, ZOOM_MAX)));
  // Accent: derive the --brass trio from the chosen accent for this family.
  // Custom themes carry their own accent; presets use the Appearance's.
  const { brass, hi, dim } = brassTrio(custom ? custom.accent : a.accent, lightFamily);
  root.style.setProperty("--brass", brass);
  root.style.setProperty("--brass-hi", hi);
  root.style.setProperty("--brass-dim", dim);
  // Fonts: inline stacks win over the :root defaults in styles.css.
  root.style.setProperty("--font-ui", uiFontStack(a.fontUi));
  root.style.setProperty("--font-mono", monoFontStack(a.fontMono));
  // Ensure the chosen webfonts are actually fetched so they resolve at boot
  // (a no-op for the default @import / system families). The terminal font is
  // a separate pref; load it here too so xterm's family is present on start.
  ensureFontLoaded(uiFontEntry(a.fontUi));
  ensureFontLoaded(monoFontEntry(a.fontMono));
  ensureFontLoaded(monoFontEntry(getTermPrefs().fontFamily));
  // The arcade theme's two faces: Bungee for the cabinet signage, Silkscreen
  // for the machine's own micro-labels. Body and code text stay on the user's
  // font choices — see the arcade section in styles.css.
  if (paletteTheme === "arcade") {
    ensureFontLoaded({ google: "Bungee" });
    ensureFontLoaded({ google: "Silkscreen" });
  }
  // Composer size knobs: a separate preference record, but applied from here so
  // main.tsx's single boot call still lands every root-level var.
  applyComposerPrefs();
  // Neutral overrides + wallpaper (or clear them when no custom theme).
  applyCustomTheme(custom);
  // Keep the browser/PWA chrome color in step with the palette. A custom
  // theme's own bg wins when it overrides it, else the base preset's bg.
  const metaColor = custom?.colors?.bg ?? THEME_BG[paletteTheme] ?? THEME_BG.dark;
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", metaColor);
}

/** Re-apply the current Appearance with a (possibly new) active custom theme.
 *  The UI calls this from an effect that watches server theme state + the
 *  APPEARANCE_EVENT: pass the resolved CustomTheme record when one is selected,
 *  or null to fall back to the presets. Thin wrapper over applyAppearance. */
export function reapplyWithCustomTheme(theme: CustomTheme | null): void {
  applyAppearance(getAppearance(), theme);
}

export function setAppearance(next: Appearance): void {
  const clamped: Appearance = {
    theme: normalizeTheme(next.theme),
    accent: normalizeAccent(next.accent),
    fontUi: normalizeFontId(next.fontUi, UI_FONTS),
    fontMono: normalizeFontId(next.fontMono, MONO_FONTS),
    uiZoom: clamp(next.uiZoom, ZOOM_MIN, ZOOM_MAX),
    customThemeId: normalizeCustomThemeId(next.customThemeId),
  };
  localStorage.setItem(A_KEY, JSON.stringify(clamped));
  // Choosing a preset (customThemeId cleared) must drop any active custom
  // theme immediately; otherwise keep the tracked record so its palette/
  // wallpaper survive an accent or font tweak made alongside it.
  if (clamped.customThemeId === null) applyAppearance(clamped, null);
  else applyAppearance(clamped);
  window.dispatchEvent(new CustomEvent<Appearance>(APPEARANCE_EVENT, { detail: clamped }));
}

export function setTermPrefs(next: TermPrefs): void {
  const clamped: TermPrefs = {
    fontSize: clamp(Math.round(next.fontSize), TFONT_MIN, TFONT_MAX),
    cursorStyle: next.cursorStyle,
    cursorBlink: next.cursorBlink,
    scrollback: clamp(Math.round(next.scrollback), SCROLLBACK_MIN, SCROLLBACK_MAX),
    fontFamily: normalizeFontId(next.fontFamily, MONO_FONTS),
  };
  localStorage.setItem(T_KEY, JSON.stringify(clamped));
  // Touching the terminal preferences is one of the three things that can flip
  // the phosphor tube; re-resolve it from the stored record just written, and
  // do it before the event so listeners read the settled answer.
  applyPhosphorFlag();
  window.dispatchEvent(new CustomEvent<TermPrefs>(TERMPREFS_EVENT, { detail: clamped }));
}
