/**
 * One-shot beacon linking "the user pressed send here" to "that message just
 * appeared in the feed there".
 *
 * Nothing is appended optimistically — a sent message only shows up once the
 * backend echoes it back as an event, so by the time the bubble mounts the
 * Composer is long done. Without this, a user bubble mounting is
 * indistinguishable from one arriving on thread load or from another device,
 * and the send entrance would fire on scrollback and on remote traffic.
 *
 * Composer sets the beacon on send; the first user bubble to mount afterwards
 * claims it, and it self-expires so a failed send doesn't decorate whatever
 * message happens to arrive next.
 */

/** Long enough for a slow round trip, short enough that a dropped send can't
 *  hand its entrance to an unrelated message arriving later. */
const WINDOW_MS = 4000;

let sentAt = 0;

type Listener = () => void;
const listeners = new Set<Listener>();

/** Composer: a message just left this client. */
export function markJustSent(): void {
  sentAt = Date.now();
  for (const listener of listeners) listener();
}

/** Watch for sends from this client.
 *
 *  Separate from `claimJustSent`, which is a one-shot the first mounting bubble
 *  consumes. The feed needs to know at the moment of the send — before the
 *  backend has echoed anything — so it can glide to the end rather than
 *  teleport there once the bubble lands. */
export function onJustSent(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Feed: is this mounting user bubble the one we just sent? Consumes the
 *  beacon, so only the first caller gets it. */
export function claimJustSent(): boolean {
  if (sentAt === 0 || Date.now() - sentAt > WINDOW_MS) return false;
  sentAt = 0;
  return true;
}
