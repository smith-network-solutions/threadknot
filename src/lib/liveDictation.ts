// Live dictation: where spoken words go in a draft the user is also typing in.
//
// The server sends each sentence twice over: a run of guesses while it is
// being spoken, then the settled text once a pause closes it. The draft holds
// one stretch of text that is still in flux, the voice region, and everything
// here is the arithmetic of keeping that stretch pointed at the right
// characters while the text around it changes under it from two directions:
// the voice rewriting the stretch, and the user editing anywhere else.
//
// Pure functions on purpose. The composer owns the state and the DOM; this
// file owns the offsets, where an off-by-one puts words in the wrong place.

import type { DictationLiveFrame } from "./protocol";

/** The stretch of the draft the voice is still rewriting: [anchor, anchor + len). */
export interface VoiceRegion {
  anchor: number;
  len: number;
}

/**
 * Write `spoken` over the voice region. `settle` ends the sentence: the region
 * collapses to an empty one just past the text, which is where the next
 * sentence starts.
 *
 * Spacing is added so words never fuse onto their neighbours, and only then:
 * nothing is added against whitespace, or before closing punctuation.
 */
export function applyVoiceText(
  text: string,
  region: VoiceRegion,
  spoken: string,
  settle: boolean,
): { text: string; region: VoiceRegion; inserted: number } {
  const before = text.slice(0, region.anchor);
  const after = text.slice(region.anchor + region.len);
  const lead = spoken && before && !/\s$/.test(before) ? " " : "";
  const trail = spoken && after && !/^[\s.,;:!?)\]}]/.test(after) ? " " : "";
  const insert = `${lead}${spoken}${trail}`;
  return {
    text: `${before}${insert}${after}`,
    region: settle
      ? { anchor: region.anchor + insert.length, len: 0 }
      : { anchor: region.anchor, len: insert.length },
    inserted: insert.length,
  };
}

/**
 * Where a caret or selection edge lands after the voice region was rewritten
 * from `region.len` characters to `inserted`.
 *
 * Ahead of the region nothing moves. At or past its end the position shifts
 * with the text, so a caret parked at the end of the spoken words rides along
 * with them and a caret further down the draft stays on its own character.
 * Inside the region it holds still unless the text shrank out from under it.
 */
export function shiftPosition(pos: number, region: VoiceRegion, inserted: number): number {
  const end = region.anchor + region.len;
  if (pos < region.anchor) return pos;
  if (pos >= end) return pos + inserted - region.len;
  return Math.min(pos, region.anchor + inserted);
}

/**
 * Move the voice region to follow an edit the user made, given the draft
 * before and after it.
 *
 * An edit ahead of the region slides it. An edit after it leaves it alone.
 * Typing exactly at its start counts as ahead, so words typed where the voice
 * is about to write stay in front of it. An edit that reaches into the region
 * stretches it over the result: that text is still a guess, and the next
 * frame for the sentence will rewrite it.
 */
export function rebaseRegion(region: VoiceRegion, before: string, after: string): VoiceRegion {
  if (before === after) return region;
  const max = Math.min(before.length, after.length);
  let prefix = 0;
  while (prefix < max && before.charCodeAt(prefix) === after.charCodeAt(prefix)) prefix += 1;
  let suffix = 0;
  while (
    suffix < max - prefix &&
    before.charCodeAt(before.length - 1 - suffix) === after.charCodeAt(after.length - 1 - suffix)
  ) {
    suffix += 1;
  }
  const changeEnd = before.length - suffix; // end of the replaced span, in `before`
  const delta = after.length - before.length;
  const regionEnd = region.anchor + region.len;

  if (changeEnd <= region.anchor) return { anchor: region.anchor + delta, len: region.len };
  if (prefix >= regionEnd) return region;

  const start = Math.min(region.anchor, prefix);
  const end = changeEnd >= regionEnd ? after.length - suffix : regionEnd + delta;
  return { anchor: start, len: Math.max(0, end - start) };
}

// ---- frame delivery ---------------------------------------------------------
// One socket, several composers. The socket publishes every frame here and the
// composer that owns the recording picks its own out by id.

type Listener = (frame: DictationLiveFrame) => void;
const listeners = new Set<Listener>();

export function publishDictationFrame(frame: DictationLiveFrame): void {
  listeners.forEach((listener) => listener(frame));
}

export function subscribeDictationFrames(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
