/**
 * One-shot beacon linking "the user pressed send here" to "that message just
 * appeared in the feed there".
 *
 * Nothing is appended optimistically — a sent message only shows up once the
 * backend echoes it back as an event, so by the time the bubble mounts the
 * Composer is long done. Without this, a user bubble mounting is
 * indistinguishable from one arriving on thread load or from another device,
 * and the arcade slam entrance would fire on scrollback and on remote traffic.
 *
 * Composer sets the beacon on send; the first user bubble to mount afterwards
 * claims it, and it self-expires so a failed send doesn't decorate whatever
 * message happens to arrive next.
 */

/** Long enough for a slow round trip, short enough that a dropped send can't
 *  hand its slam to an unrelated message arriving later. */
const WINDOW_MS = 4000;

let sentAt = 0;

/** Composer: a message just left this client. */
export function markJustSent(): void {
  sentAt = Date.now();
}

/** Feed: is this mounting user bubble the one we just sent? Consumes the
 *  beacon, so only the first caller gets it. */
export function claimJustSent(): boolean {
  if (sentAt === 0 || Date.now() - sentAt > WINDOW_MS) return false;
  sentAt = 0;
  return true;
}
