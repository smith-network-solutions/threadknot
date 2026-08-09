// Model portraits: the picture a sidebar thread card wears for the MODEL behind
// it (the agent it belongs to is only the fallback). Persisted to localStorage
// and broadcast on the window so every open card re-resolves without a reload.
// Mirrors the lightweight preference pattern in appearance.ts: read + normalize
// -> setItem -> CustomEvent.

const P_KEY = "threadknot.portraits";

/** Fired after any portrait is set or cleared. detail: the new PortraitPrefs. */
export const PORTRAITS_EVENT = "threadknot:portraits";

export interface PortraitPrefs {
  /** Keyed either by a model id ("claude-opus-5") or by the per-agent fallback
   *  key "agent:<id>" ("agent:claude"). Values are image data URLs, so a
   *  portrait is self-contained and never reaches for a file that has moved. */
  byKey: Record<string, string>;
}

/** The key an agent's own fallback portrait is stored under. One place owns the
 *  prefix so the settings UI and resolvePortrait can never drift apart. */
export function agentPortraitKey(agent: string): string {
  return `agent:${agent}`;
}

// Parsed once and held until a write, so the sidebar can resolve a portrait per
// card render without re-parsing storage each time. Every read hands back a
// copy, so a caller mutating its result cannot poison the cache.
let cached: Record<string, string> | null = null;

/** Only data URLs are kept: a stored http(s) reference would leak where the app
 *  is and fail offline, and anything else is a hand-edited value we cannot
 *  render. */
function isDataUrl(value: unknown): value is string {
  return typeof value === "string" && value.startsWith("data:");
}

/** Drop anything that is not a key pointing at a data URL (older build, junk
 *  written by hand) rather than rendering a broken image. */
function normalizeByKey(value: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!value || typeof value !== "object") return out;
  for (const [key, v] of Object.entries(value as Record<string, unknown>)) {
    if (key && isDataUrl(v)) out[key] = v;
  }
  return out;
}

export function getPortraits(): PortraitPrefs {
  if (!cached) {
    let byKey: Record<string, string> = {};
    try {
      const raw = localStorage.getItem(P_KEY);
      if (raw) byKey = normalizeByKey((JSON.parse(raw) as Partial<PortraitPrefs>).byKey);
    } catch {
      /* no storage, or unparseable: nobody has a portrait */
    }
    cached = byKey;
  }
  return { byKey: { ...cached } };
}

/**
 * Set (or, with a null url, clear) one portrait, persist the whole record and
 * announce it. A value that is not a data URL is ignored rather than stored,
 * so the same rule guards the write and the read.
 */
export function setPortrait(key: string, dataUrl: string | null): void {
  if (!key) return;
  const next = getPortraits();
  if (dataUrl === null) delete next.byKey[key];
  else if (isDataUrl(dataUrl)) next.byKey[key] = dataUrl;
  else return;
  try {
    localStorage.setItem(P_KEY, JSON.stringify(next));
    cached = { ...next.byKey };
  } catch {
    // Storage full or locked down. The cache is left alone deliberately: the
    // event below makes every listener re-read, so the UI shows what actually
    // survives rather than a change that would vanish on the next launch.
  }
  window.dispatchEvent(new CustomEvent<PortraitPrefs>(PORTRAITS_EVENT, { detail: next }));
}

/** The portrait for a chat: the model's own picture, else the agent's fallback,
 *  else nothing (the card keeps whatever mark it already renders). */
export function resolvePortrait(model: string | undefined, agent: string): string | null {
  const { byKey } = getPortraits();
  return (model ? byKey[model] : undefined) ?? byKey[agentPortraitKey(agent)] ?? null;
}
