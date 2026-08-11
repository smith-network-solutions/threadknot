// Was that run played by a person?
//
// Read this before trusting the answer: it is a HEURISTIC, not a proof. There
// is no way for a web page to prove a human pressed a key, and anyone who wants
// to defeat this badly enough will. What it does do is separate an ordinary
// human run from the two kinds of cheating that actually happen here: a script
// dispatching synthetic KeyboardEvents, and an agent driving the app through a
// browser automation channel.
//
// Three signals, in order of how hard they are to fake:
//
//   1. `event.isTrusted`. False for anything created by dispatchEvent, which
//      rules out the one-line console cheat entirely. A real automation driver
//      (CDP and friends) injects at the browser's input layer and produces
//      trusted events, so this is a floor, not a ceiling.
//   2. Timing texture. A person's gaps between keypresses are noisy: the
//      coefficient of variation of the intervals sits well above zero, and hold
//      durations scatter. A loop driving the game on a fixed tick, or an agent
//      issuing one input per frame, produces intervals that are far too even.
//      This is the signal that catches an agent.
//   3. Volume. Nine levels is thousands of keystrokes. A run that reaches the
//      end on a hundred events did not play the games.
//
// Everything is computed from counters kept as the events arrive, so watching
// costs a few additions per keypress and no allocation.

export type Attestation = "human" | "unverified";

export interface WitnessReport {
  verdict: Attestation;
  /** Why it came out that way, in plain language, for the badge tooltip. */
  reasons: string[];
  events: number;
  /** Coefficient of variation of the gaps between keypresses. */
  intervalCv: number;
  /** Coefficient of variation of how long keys were held down. */
  holdCv: number;
  untrusted: number;
}

/** Below this many key events a campaign simply was not played by hand. */
const MIN_EVENTS = 400;
/** A person's inter-key gaps scatter; a tick loop's do not. */
const MIN_INTERVAL_CV = 0.35;
/** Same idea for how long a key stays down. */
const MIN_HOLD_CV = 0.2;
/** Gaps longer than this are thinking time, not rhythm, and would swamp the
 *  variance measure with a signal that says nothing about who is typing. */
const MAX_INTERVAL_MS = 2000;

/**
 * Accumulates the shape of the input for one campaign. One instance per run;
 * a new run starts a new witness, so a perfect campaign is judged only on the
 * keystrokes that produced it.
 */
export class Witness {
  private events = 0;
  private untrusted = 0;

  // Running mean/variance for gaps between keydowns (Welford, so a long run
  // cannot drift and nothing has to be stored).
  private iN = 0;
  private iMean = 0;
  private iM2 = 0;
  private lastDown = 0;

  // Same for how long each key was held.
  private hN = 0;
  private hMean = 0;
  private hM2 = 0;
  private downAt = new Map<string, number>();

  /** Set once anything arrives that could not have come from a keyboard. */
  private tainted = false;

  keyDown(code: string, trusted: boolean, at: number): void {
    this.events++;
    if (!trusted) {
      this.untrusted++;
      this.tainted = true;
    }
    if (!this.downAt.has(code)) this.downAt.set(code, at);

    if (this.lastDown > 0) {
      const gap = at - this.lastDown;
      // Ignore the pauses between screens: they are not part of the rhythm.
      if (gap > 0 && gap <= MAX_INTERVAL_MS) {
        this.iN++;
        const d = gap - this.iMean;
        this.iMean += d / this.iN;
        this.iM2 += d * (gap - this.iMean);
      }
    }
    this.lastDown = at;
  }

  keyUp(code: string, trusted: boolean, at: number): void {
    if (!trusted) {
      this.untrusted++;
      this.tainted = true;
    }
    const down = this.downAt.get(code);
    this.downAt.delete(code);
    if (down === undefined) return;
    const held = at - down;
    if (held < 0 || held > MAX_INTERVAL_MS) return;
    this.hN++;
    const d = held - this.hMean;
    this.hMean += d / this.hN;
    this.hM2 += d * (held - this.hMean);
  }

  private cv(n: number, mean: number, m2: number): number {
    if (n < 8 || mean <= 0) return 0;
    return Math.sqrt(m2 / (n - 1)) / mean;
  }

  report(): WitnessReport {
    const intervalCv = this.cv(this.iN, this.iMean, this.iM2);
    const holdCv = this.cv(this.hN, this.hMean, this.hM2);
    const reasons: string[] = [];

    if (this.tainted) reasons.push("input included events the browser did not mark as real");
    if (this.events < MIN_EVENTS) reasons.push("too few keystrokes for nine levels played by hand");
    if (intervalCv < MIN_INTERVAL_CV) reasons.push("the gaps between keystrokes were too even");
    if (holdCv < MIN_HOLD_CV) reasons.push("keys were held for too uniform a time");

    return {
      verdict: reasons.length === 0 ? "human" : "unverified",
      reasons,
      events: this.events,
      intervalCv: Math.round(intervalCv * 100) / 100,
      holdCv: Math.round(holdCv * 100) / 100,
      untrusted: this.untrusted,
    };
  }
}
