// The hidden circuit behind the About screen: the entry sequence, and the one
// fact the run leaves behind.
//
// Two things live here rather than in the components that use them. The matcher
// is pure (a progress index in, a progress index out) so the rules can be read
// and reasoned about without a DOM or a running game. The award record is read
// from the sidebar chrome and the skins card, neither of which should have to
// import a game to find out whether a crest is due.
//
// Nothing here touches the server. Finding the circuit is a per-device fact on
// purpose: it is something you did at this keyboard, and it would mean nothing
// arriving pre-earned on a phone you paired last week.

/** One rung of the entry sequence. */
export interface CircuitStep {
  /** Layout-independent physical key. The primary match: a user on a
   *  non-QWERTY layout still presses the same two buttons at the end. */
  code: string;
  /** `event.key` fallback, upper-cased. Some webviews report an empty `code`
   *  for synthesised or remapped events, and a sequence that silently cannot be
   *  entered on one machine is worse than one that accepts two spellings. */
  key: string;
  /** What the contact strip shows for this rung once it is lit. */
  glyph: string;
}

/** Up, up, down, down, left, right, left, right, B, A, Enter: the cadence every
 *  player of a certain era has in their hands before they have it in their head.
 *  Held under Ctrl+Shift so no part of it can be entered by accident. */
export const CIRCUIT_SEQUENCE: readonly CircuitStep[] = [
  { code: "ArrowUp", key: "ARROWUP", glyph: "↑" },
  { code: "ArrowUp", key: "ARROWUP", glyph: "↑" },
  { code: "ArrowDown", key: "ARROWDOWN", glyph: "↓" },
  { code: "ArrowDown", key: "ARROWDOWN", glyph: "↓" },
  { code: "ArrowLeft", key: "ARROWLEFT", glyph: "←" },
  { code: "ArrowRight", key: "ARROWRIGHT", glyph: "→" },
  { code: "ArrowLeft", key: "ARROWLEFT", glyph: "←" },
  { code: "ArrowRight", key: "ARROWRIGHT", glyph: "→" },
  { code: "KeyB", key: "B", glyph: "B" },
  { code: "KeyA", key: "A", glyph: "A" },
  { code: "Enter", key: "ENTER", glyph: "⏎" },
];

export const CIRCUIT_LENGTH = CIRCUIT_SEQUENCE.length;

/** Keys that are part of the chord itself. Releasing or re-pressing a modifier
 *  mid-sequence must not count as a wrong answer: you are holding it down. */
const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "CapsLock"]);

export type CircuitVerdict =
  /** Not addressed to the circuit at all. Progress is untouched and the event
   *  should be left for whatever else wants it. */
  | { kind: "ignored" }
  /** A correct rung. `progress` is the new count of lit contacts. */
  | { kind: "advanced"; progress: number }
  /** A wrong key while the chord was held. Progress is reset (to 1 when the
   *  wrong key happens to be a fresh first rung, which is how a fumbled entry
   *  recovers without lifting the modifiers). The event was a Ctrl+Shift chord
   *  aimed at this screen, so the caller should consume it. */
  | { kind: "reset"; progress: number }
  /** The chord was let go part-way through: the attempt is over, but the key
   *  itself belongs to the app. Distinct from `reset` precisely so the caller
   *  knows NOT to swallow it: otherwise a Tab pressed halfway through an
   *  abandoned attempt would silently do nothing. */
  | { kind: "abandoned" }
  /** The last rung landed. */
  | { kind: "unlocked" };

/** Does this event match the step at `index`? */
function matches(e: KeyboardEvent, index: number): boolean {
  const step = CIRCUIT_SEQUENCE[index];
  if (!step) return false;
  if (e.code) return e.code === step.code;
  return (e.key ?? "").toUpperCase() === step.key;
}

/**
 * Feed one keydown to the matcher.
 *
 * The chord is strict: both Ctrl and Shift down, neither Alt nor Meta, and no
 * auto-repeat (holding an arrow must not walk the sequence on its own). Events
 * that fail the chord are `ignored` rather than `reset`, so ordinary typing
 * elsewhere on the screen never has to be filtered out by the caller: only a
 * genuine Ctrl+Shift chord can break a run in progress.
 */
export function advanceCircuit(progress: number, e: KeyboardEvent): CircuitVerdict {
  if (e.repeat) return { kind: "ignored" };
  if (MODIFIER_KEYS.has(e.key)) return { kind: "ignored" };
  if (!e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) {
    // A chord that lost a modifier part-way through is a failed attempt, not
    // background noise: drop what was accumulated so a half-entered sequence
    // cannot be finished five minutes later. The key still belongs to whatever
    // it was meant for, which is what separates this from a `reset`.
    return progress > 0 ? { kind: "abandoned" } : { kind: "ignored" };
  }

  const at = Math.max(0, Math.min(progress, CIRCUIT_LENGTH));
  if (matches(e, at)) {
    const next = at + 1;
    return next >= CIRCUIT_LENGTH ? { kind: "unlocked" } : { kind: "advanced", progress: next };
  }
  return { kind: "reset", progress: matches(e, 0) ? 1 : 0 };
}

/* -------------------------------------------------------------------------- */
/* The award                                                                   */
/* -------------------------------------------------------------------------- */

const AWARD_KEY = "threadknot.legacyCircuit.v1";

/** Fired on every write so the crest in the sidebar and the crest on the skins
 *  card light up the moment the run ends, without either of them polling. */
export const LEGACY_AWARD_EVENT = "threadknot:legacyaward";

export interface LegacyAward {
  /** All three stages cleared at least once on this device. */
  earned: boolean;
  /** ISO timestamp of the first completion, or "" while unearned. */
  earnedAt: string;
  /** Best campaign score on this device, earned or not. */
  bestScore: number;
  /** Sound preference for the cabinet. Kept with the award because it is the
   *  only screen that makes noise, and it should be remembered between runs. */
  sound: boolean;
}

const AWARD_DEFAULT: LegacyAward = {
  earned: false,
  earnedAt: "",
  bestScore: 0,
  sound: true,
};

/** Storage is a string a user can hand-edit and an older build can have written
 *  a different shape into, so every field is re-derived rather than trusted. */
function normalize(raw: unknown): LegacyAward {
  if (!raw || typeof raw !== "object") return { ...AWARD_DEFAULT };
  const r = raw as Partial<LegacyAward>;
  const best = Number(r.bestScore);
  return {
    earned: r.earned === true,
    earnedAt: typeof r.earnedAt === "string" ? r.earnedAt : "",
    bestScore: Number.isFinite(best) && best > 0 ? Math.floor(best) : 0,
    sound: r.sound !== false,
  };
}

export function getLegacyAward(): LegacyAward {
  try {
    const raw = localStorage.getItem(AWARD_KEY);
    return normalize(raw ? JSON.parse(raw) : null);
  } catch {
    // Private-mode storage, a quota wall, or malformed JSON: an unearned crest
    // is the right answer to every one of those.
    return { ...AWARD_DEFAULT };
  }
}

function writeAward(next: LegacyAward): LegacyAward {
  try {
    localStorage.setItem(AWARD_KEY, JSON.stringify(next));
  } catch {
    /* the run still finishes; only the memory of it is lost */
  }
  window.dispatchEvent(new CustomEvent<LegacyAward>(LEGACY_AWARD_EVENT, { detail: next }));
  return next;
}

/** Record a finished campaign. The timestamp is set once, on the first win, so
 *  a replay never rewrites the day it was found. */
export function awardLegacyCrest(score: number): LegacyAward {
  const cur = getLegacyAward();
  return writeAward({
    ...cur,
    earned: true,
    earnedAt: cur.earnedAt || new Date().toISOString(),
    bestScore: Math.max(cur.bestScore, Math.floor(Math.max(0, score))),
  });
}

/** Record a score from a run that ended short of the crest. */
export function recordLegacyScore(score: number): LegacyAward {
  const cur = getLegacyAward();
  const best = Math.max(cur.bestScore, Math.floor(Math.max(0, score)));
  if (best === cur.bestScore) return cur;
  return writeAward({ ...cur, bestScore: best });
}

export function setLegacySound(sound: boolean): LegacyAward {
  return writeAward({ ...getLegacyAward(), sound });
}

/** The crest's name, in one place: the HUD, the tooltip and the completion
 *  screen all have to agree on it. */
export const CREST_NAME = "LEGACY WEAVER";

/** The dedication. Shown on the way in and again on the way out, because it is
 *  the reason the rest of this exists. */
export const CIRCUIT_TRIBUTE =
  "This hidden circuit is an homage to the developers and gamers who came before us: " +
  "the people who turned limitations into worlds, bugs into lessons, and code into play.";
