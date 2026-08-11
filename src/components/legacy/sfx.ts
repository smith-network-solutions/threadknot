// The cabinet's voice: square waves and an envelope, nothing sampled.
//
// Every sound is synthesised from an oscillator here and now, so the app ships
// no audio files and there is no chance of a recognisable riff riding along in
// the bundle. The AudioContext is created on the first note rather than at
// import time, because a context built before a user gesture starts suspended
// and browsers rightly complain about it.

import type { Cue } from "./stages";

interface Note {
  /** Starting frequency in Hz. */
  hz: number;
  /** Ending frequency; a slide from `hz` when it differs. */
  to?: number;
  /** Seconds. */
  dur: number;
  type: OscillatorType;
  gain: number;
}

/** One voice per cue. Kept short and dry: these fire many times a second in
 *  KNOTLINE, and anything with a tail turns into mush. */
const VOICES: Record<Cue, Note> = {
  step: { hz: 90, dur: 0.022, type: "square", gain: 0.035 },
  pickup: { hz: 620, to: 1180, dur: 0.1, type: "square", gain: 0.11 },
  shoot: { hz: 900, to: 340, dur: 0.07, type: "square", gain: 0.08 },
  hit: { hz: 320, to: 90, dur: 0.11, type: "sawtooth", gain: 0.1 },
  gate: { hz: 440, to: 880, dur: 0.22, type: "triangle", gain: 0.12 },
  clear: { hz: 520, to: 1560, dur: 0.34, type: "square", gain: 0.12 },
  die: { hz: 300, to: 55, dur: 0.5, type: "sawtooth", gain: 0.14 },
  // A gun going up should sound like it: a short confident climb, distinct
  // from the pickup blip so you know which of the two you just caught.
  power: { hz: 380, to: 1500, dur: 0.2, type: "square", gain: 0.13 },
  boom: { hz: 220, to: 32, dur: 0.6, type: "sawtooth", gain: 0.17 },
};

export class Cabinet {
  private ctx: AudioContext | null = null;
  private master: GainNode | null = null;
  private muted = false;
  private dead = false;

  constructor(muted: boolean) {
    this.muted = muted;
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    if (this.master && this.ctx) {
      this.master.gain.setTargetAtTime(muted ? 0 : 1, this.ctx.currentTime, 0.01);
    }
  }

  /** Called from the keypress that starts a run, so the context is born inside
   *  a gesture and never has to be resumed. */
  arm(): void {
    this.ensure();
    void this.ctx?.resume().catch(() => undefined);
  }

  private ensure(): AudioContext | null {
    if (this.dead) return null;
    if (this.ctx) return this.ctx;
    try {
      const Ctor =
        window.AudioContext ??
        (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
      if (!Ctor) throw new Error("no AudioContext");
      const ctx = new Ctor();
      const master = ctx.createGain();
      master.gain.value = this.muted ? 0 : 1;
      master.connect(ctx.destination);
      this.ctx = ctx;
      this.master = master;
      return ctx;
    } catch {
      // A webview with audio disabled should cost the player a silent game, not
      // a broken one. Give up permanently rather than retrying every frame.
      this.dead = true;
      return null;
    }
  }

  play(cue: Cue): void {
    if (this.muted) return;
    const ctx = this.ensure();
    const master = this.master;
    if (!ctx || !master || ctx.state === "closed") return;
    const v = VOICES[cue];
    const now = ctx.currentTime;
    try {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = v.type;
      osc.frequency.setValueAtTime(v.hz, now);
      if (v.to !== undefined) osc.frequency.exponentialRampToValueAtTime(Math.max(1, v.to), now + v.dur);
      // A hard stop clicks; a short ramp to (near) zero does not.
      gain.gain.setValueAtTime(0.0001, now);
      gain.gain.exponentialRampToValueAtTime(v.gain, now + 0.008);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + v.dur);
      osc.connect(gain);
      gain.connect(master);
      osc.start(now);
      osc.stop(now + v.dur + 0.02);
    } catch {
      /* one dropped bleep is not worth interrupting a run over */
    }
  }

  /** The rising figure the crest lands on. Notes are scheduled ahead rather
   *  than timed from JS so the phrase holds together under a busy main thread. */
  fanfare(): void {
    if (this.muted) return;
    const ctx = this.ensure();
    const master = this.master;
    if (!ctx || !master) return;
    const steps = [523.25, 659.25, 783.99, 1046.5, 1318.5];
    steps.forEach((hz, i) => {
      try {
        const at = ctx.currentTime + i * 0.13;
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.type = "square";
        osc.frequency.setValueAtTime(hz, at);
        gain.gain.setValueAtTime(0.0001, at);
        gain.gain.exponentialRampToValueAtTime(0.1, at + 0.02);
        gain.gain.exponentialRampToValueAtTime(0.0001, at + (i === steps.length - 1 ? 0.7 : 0.16));
        osc.connect(gain);
        gain.connect(master);
        osc.start(at);
        osc.stop(at + 0.75);
      } catch {
        /* as above */
      }
    });
  }

  /** The tube powering up: a descending whistle under the boot animation. */
  power(): void {
    if (this.muted) return;
    const ctx = this.ensure();
    const master = this.master;
    if (!ctx || !master) return;
    try {
      const now = ctx.currentTime;
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "triangle";
      osc.frequency.setValueAtTime(1800, now);
      osc.frequency.exponentialRampToValueAtTime(120, now + 0.55);
      gain.gain.setValueAtTime(0.0001, now);
      gain.gain.exponentialRampToValueAtTime(0.07, now + 0.03);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.6);
      osc.connect(gain);
      gain.connect(master);
      osc.start(now);
      osc.stop(now + 0.65);
    } catch {
      /* as above */
    }
  }

  dispose(): void {
    const ctx = this.ctx;
    this.ctx = null;
    this.master = null;
    // Closing is async and can reject if the context is already gone; there is
    // nothing to do about either.
    void ctx?.close().catch(() => undefined);
  }
}
