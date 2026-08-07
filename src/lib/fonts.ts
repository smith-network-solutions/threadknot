// On-demand Google Fonts loading. Shipping ~40 families at startup is
// unacceptable, so a family's stylesheet is injected only when something
// actually needs it — the active fonts at boot (from applyAppearance) and every
// visible option when a FontPicker list opens. Each family loads at most once,
// tracked in a module Map of load promises. The default Archivo / JetBrains Mono faces come
// through the @import in styles.css and carry no `google` field, so they never
// round-trip here and the default look never flashes.

/** The minimal shape the loader reads: a Google Fonts family query
 *  (e.g. "Space+Grotesk:wght@400;600"), or none for system-stack fonts that
 *  need no webfont. Structurally satisfied by appearance.ts's FontOption. */
export interface LoadableFont {
  google?: string;
}

// The resolved load promise per family, so a family is requested at most once
// and repeat callers (a picker reopening, the terminal re-measuring) share the
// one round trip rather than starting another.
const loading = new Map<string, Promise<void>>();

// Longest a caller will wait on a family before giving up, so a slow or dead
// CDN never hangs an awaiting caller (the terminal re-measure). The <link>
// still lands whenever it lands; only the promise is capped.
const LOAD_TIMEOUT_MS = 3000;

/** The CSS family name the css2 query resolves to (for document.fonts.load):
 *  the part before ":", with "+" as spaces — e.g.
 *  "Space+Grotesk:wght@400;600" -> "Space Grotesk". */
function familyName(query: string): string {
  return query.split(":")[0].replace(/\+/g, " ");
}

/** Inject a family's stylesheet <link> once and resolve when the family is
 *  actually usable (its faces have loaded), so a caller that must re-measure
 *  after the swap can await it. No-op (resolved) for system-stack entries (no
 *  `google`). display=swap still renders the fallback first, so nothing blocks
 *  on the network for fire-and-forget callers. */
export function ensureFontLoaded(entry: LoadableFont | undefined): Promise<void> {
  const family = entry?.google;
  if (!family) return Promise.resolve();
  const existing = loading.get(family);
  if (existing) return existing;
  if (typeof document === "undefined") return Promise.resolve();

  const promise = new Promise<void>((resolve) => {
    const done = () => resolve();
    const link = document.createElement("link");
    link.rel = "stylesheet";
    // The family query already carries the ":" / ";" / "@" the css2 API expects
    // literally (the same shape styles.css's @import uses), so it is not encoded.
    link.href = `https://fonts.googleapis.com/css2?family=${family}&display=swap`;
    // Resolve once the faces are truly usable: wait for the stylesheet, then ask
    // the FontFaceSet to load the family so metrics are correct. A timeout backs
    // the whole thing so a failed CDN resolves (falls back) instead of hanging.
    const settle = () => {
      const fonts = (document as Document & { fonts?: FontFaceSet }).fonts;
      if (!fonts) return done();
      fonts.load(`12px "${familyName(family)}"`).then(done, done);
    };
    link.addEventListener("load", settle);
    link.addEventListener("error", done);
    document.head.appendChild(link);
    window.setTimeout(done, LOAD_TIMEOUT_MS);
  });
  loading.set(family, promise);
  return promise;
}

/** Fire the loader for a whole list (used when a picker opens: the CSS files
 *  are small and cache, so requesting all visible previews at once is fine). */
export function ensureFontsLoaded(entries: readonly LoadableFont[]): void {
  for (const entry of entries) ensureFontLoaded(entry);
}
