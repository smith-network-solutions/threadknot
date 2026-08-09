// The curated skin catalog: the copy, screenshots and module list the Skins
// settings block renders. Kept out of appearance.ts so the marketing-side of a
// skin (descriptions, previews, where to find more) can grow without touching
// the preference core, and so appearance.ts stays free of asset imports.

import type { SkinModule } from "./appearance";
// Vite resolves these to hashed URLs at build time, so the previews are part of
// the bundle rather than a runtime fetch that could 404 offline.
import characterSelectShot from "../assets/skins/retro-tech/character-select.png";
import roundSplashShot from "../assets/skins/retro-tech/round-splash.png";

export interface SkinModuleInfo {
  id: SkinModule;
  label: string;
  /** One line, plain language: what the user loses by switching it off. */
  hint: string;
}

/** Presented in this order everywhere: the settings checklist and the preview
 *  modal read from this one list. */
export const SKIN_MODULES: readonly SkinModuleInfo[] = [
  {
    id: "usage",
    label: "usage bars",
    hint: "Show the context and rate-limit meters as vitality bars.",
  },
  {
    id: "parley",
    label: "review & rounds",
    hint: "Pick reviewers from a character-select roster, with ROUND N call-outs.",
  },
  {
    id: "cards",
    label: "sidebar cards",
    hint: "Theme sidebar cards, the project rail and the settled shelf. Off keeps your current sidebar format.",
  },
  {
    id: "terminal",
    label: "terminal",
    hint: "Repaint the terminal in green phosphor (only while your terminal settings are untouched).",
  },
  {
    id: "workspace",
    label: "workspace panels",
    hint: "Frame the Files, Git, Artifacts, Browser and Terminal panes in cabinet chrome.",
  },
];

export interface SkinScreenshot {
  src: string;
  caption: string;
}

export interface CuratedSkin {
  /** The THEMES id this skin applies. */
  id: string;
  name: string;
  /** One paragraph, shown on hover and in the preview modal. */
  description: string;
  screenshots: SkinScreenshot[];
}

export const CURATED_SKINS: readonly CuratedSkin[] = [
  {
    id: "arcade",
    name: "Retro-Tech",
    description:
      "Neon cabinet chrome over the whole app: vitality-bar usage meters, a character-select reviewer roster with ROUND N call-outs, a green phosphor terminal and pixel signage throughout. Every part is a separate switch, so you can take the cabinet and leave the rest.",
    screenshots: [
      { src: characterSelectShot, caption: "Reviewer roster, character-select style" },
      { src: roundSplashShot, caption: "ROUND N splash when a review kicks off" },
    ],
  },
];

/** Where community skins will live. Only a link for now: nothing in the app
 *  fetches from it yet. */
export const SKIN_MARKET_URL = "https://github.com/smith-network-solutions/threadknot-skins";
