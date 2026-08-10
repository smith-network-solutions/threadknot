// The thing behind the sequence: a cabinet that takes over the settings screen.
//
// It owns the campaign (three stages, three shared lives, one score), the frame
// around it (the tube powering on, the marquee, the HUD) and the input. The
// stages themselves know none of that; see stages.ts for the contract.
//
// Two deliberate shapes in here:
//
//   * The loop writes the fast-moving HUD text straight to DOM nodes through
//     refs. A stage readout changes sixty times a second and re-rendering this
//     whole tree that often, next to a canvas, is the one thing that would make
//     the games feel worse than the machines they are dedicated to.
//   * Everything the loop touches lives in a ref, so starting and stopping it
//     never depends on a closure catching the right render.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  awardLegacyCrest,
  CIRCUIT_TRIBUTE,
  CREST_NAME,
  getLegacyAward,
  recordLegacyScore,
  setLegacySound,
} from "../../lib/legacyCircuit";
import { ensureFontLoaded } from "../../lib/fonts";
import { Cabinet } from "./sfx";
import { Crest } from "./Crest";
import { STAGES, VIEW_H, VIEW_W, type Btn, type Input, type StageRun } from "./stages";
import "../../styles/legacy.css";

type Phase =
  /** The tube warming up. */
  | "boot"
  /** Marquee, dedication, insert coin. */
  | "title"
  /** Attract card for the stage about to start. */
  | "intro"
  | "play"
  /** A life gone; the stage restarts from the top. */
  | "lost"
  | "stage-clear"
  /** Out of lives. */
  | "over"
  /** All three cleared. */
  | "win";

/** Physical keys to game buttons. Codes, not characters, so the layout under
 *  the keyboard does not decide whether the game is playable. */
const BUTTONS: Record<string, Btn> = {
  ArrowUp: "up",
  KeyW: "up",
  ArrowDown: "down",
  KeyS: "down",
  ArrowLeft: "left",
  KeyA: "left",
  ArrowRight: "right",
  KeyD: "right",
  Space: "fire",
  KeyZ: "fire",
  KeyJ: "fire",
};

const LIVES = 3;
/** Never advance a stage by more than this in one tick. A backgrounded webview
 *  hands back a multi-second delta, and replaying it in one step is a death
 *  the player never saw coming. */
const MAX_STEP = 1 / 30;

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

export function LegacyCircuit({ onExit }: { onExit: () => void }) {
  const calm = useMemo(prefersReducedMotion, []);
  const [phase, setPhase] = useState<Phase>(calm ? "title" : "boot");
  const [stageIndex, setStageIndex] = useState(0);
  const [lives, setLives] = useState(LIVES);
  const [banked, setBanked] = useState(0);
  const [muted, setMuted] = useState(() => !getLegacyAward().sound);
  const [paused, setPaused] = useState(false);
  const [best, setBest] = useState(() => getLegacyAward().bestScore);
  const [scale, setScale] = useState(2);

  const stage = STAGES[Math.min(stageIndex, STAGES.length - 1)];

  const frameRef = useRef<HTMLDivElement | null>(null);
  const stageWrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const readoutRef = useRef<HTMLSpanElement | null>(null);
  const scoreRef = useRef<HTMLSpanElement | null>(null);

  const runRef = useRef<StageRun | null>(null);
  const rafRef = useRef<number | null>(null);
  const phaseRef = useRef<Phase>(phase);
  const pausedRef = useRef(false);
  const bankedRef = useRef(0);
  const heldRef = useRef<Set<Btn>>(new Set());
  const tappedRef = useRef<Set<Btn>>(new Set());
  const cabinetRef = useRef<Cabinet | null>(null);
  const seedRef = useRef<number>(Date.now() >>> 0);
  /** Highest total this run has reached, so exiting mid-stage still records
   *  what was actually played rather than only the banked stages. */
  const peakRef = useRef(0);
  /** Whether this stage's outcome has already been acted on. The loop keeps
   *  running for a frame or two after a stage ends (the phase change has to
   *  round-trip through React before the effect tears the loop down), and
   *  without this a single death spends two lives. */
  const settledRef = useRef(false);

  phaseRef.current = phase;
  pausedRef.current = paused;
  bankedRef.current = banked;

  if (!cabinetRef.current) cabinetRef.current = new Cabinet(muted);

  // The cabinet's own two faces. They are normally fetched only when the
  // Retro-Tech palette is applied, and this screen can be opened from any
  // theme, so it asks for them itself.
  useEffect(() => {
    void ensureFontLoaded({ google: "Silkscreen" });
    void ensureFontLoaded({ google: "Bungee" });
    frameRef.current?.focus();
  }, []);

  useEffect(() => {
    const cabinet = cabinetRef.current;
    return () => cabinet?.dispose();
  }, []);

  /* ---- layout ---------------------------------------------------------- */

  // Whole-number scaling only: a 1.5x canvas is a blurry canvas, and the entire
  // point of a 320x240 buffer is that every pixel lands on a whole number of
  // real ones.
  useEffect(() => {
    const wrap = stageWrapRef.current;
    if (!wrap) return;
    const measure = () => {
      const r = wrap.getBoundingClientRect();
      if (r.width < 4 || r.height < 4) return;
      const fit = Math.min(r.width / VIEW_W, r.height / VIEW_H);
      setScale(Math.max(1, Math.min(3, Math.floor(fit))));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(wrap);
    return () => ro.disconnect();
  }, []);

  /* ---- the loop -------------------------------------------------------- */

  const paint = useCallback(() => {
    const canvas = canvasRef.current;
    const run = runRef.current;
    if (!canvas || !run) return;
    const g = canvas.getContext("2d");
    if (!g) return;
    g.imageSmoothingEnabled = false;
    run.draw(g);
  }, []);

  const startStage = useCallback(
    (index: number) => {
      seedRef.current = (seedRef.current * 1664525 + 1013904223) >>> 0;
      runRef.current = STAGES[index].create(seedRef.current);
      heldRef.current.clear();
      tappedRef.current.clear();
      settledRef.current = false;
      // The HUD is loop-driven, and the loop is not running yet: seed the
      // readout here so an attract card never sits above the previous stage's
      // status line.
      if (readoutRef.current) readoutRef.current.textContent = runRef.current.readout();
      paint();
    },
    [paint],
  );

  useEffect(() => {
    if (phase !== "play") {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
      return;
    }

    let last = performance.now();
    const tick = (now: number) => {
      rafRef.current = requestAnimationFrame(tick);
      const dt = Math.min(MAX_STEP, Math.max(0, (now - last) / 1000));
      last = now;

      const run = runRef.current;
      if (!run) return;

      if (!pausedRef.current && !settledRef.current) {
        const input: Input = { held: heldRef.current, tapped: tappedRef.current };
        run.update(dt, input);
        tappedRef.current.clear();
        for (const cue of run.drain()) cabinetRef.current?.play(cue);
      }

      const g = canvasRef.current?.getContext("2d");
      if (g) {
        g.imageSmoothingEnabled = false;
        run.draw(g);
      }

      const total = bankedRef.current + run.score;
      peakRef.current = Math.max(peakRef.current, total);
      if (readoutRef.current) readoutRef.current.textContent = run.readout();
      if (scoreRef.current) scoreRef.current.textContent = String(total).padStart(6, "0");

      if (!settledRef.current && run.status === "cleared") {
        settledRef.current = true;
        setBanked((s) => s + run.score);
        setPhase(stageIndex >= STAGES.length - 1 ? "win" : "stage-clear");
      } else if (!settledRef.current && run.status === "failed") {
        settledRef.current = true;
        setLives((n) => Math.max(0, n - 1));
        setPhase(lives <= 1 ? "over" : "lost");
      }
    };

    rafRef.current = requestAnimationFrame(tick);
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    };
    // `lives` is read to decide game-over; `stageIndex` to decide the last
    // stage. Both are stable for the length of a stage, so re-arming the loop
    // when they change is correct and costs one frame.
  }, [phase, lives, stageIndex]);

  /* ---- outcomes -------------------------------------------------------- */

  useEffect(() => {
    if (phase === "win") {
      const total = bankedRef.current;
      peakRef.current = Math.max(peakRef.current, total);
      const award = awardLegacyCrest(total);
      setBest(award.bestScore);
      cabinetRef.current?.fanfare();
    } else if (phase === "over") {
      const award = recordLegacyScore(peakRef.current);
      setBest(award.bestScore);
    }
  }, [phase]);

  // Leaving part-way through still counts what was played.
  useEffect(() => {
    return () => {
      if (peakRef.current > 0) recordLegacyScore(peakRef.current);
    };
  }, []);

  const beginRun = useCallback(() => {
    setBanked(0);
    bankedRef.current = 0;
    setLives(LIVES);
    setStageIndex(0);
    peakRef.current = 0;
    startStage(0);
    setPhase("intro");
  }, [startStage]);

  /** Enter on any card: whatever the obvious next thing is. */
  const advance = useCallback(() => {
    cabinetRef.current?.arm();
    switch (phaseRef.current) {
      case "title":
        beginRun();
        break;
      case "intro":
        setPaused(false);
        setPhase("play");
        break;
      case "lost":
        startStage(stageIndex);
        setPhase("intro");
        break;
      case "stage-clear": {
        const next = stageIndex + 1;
        setStageIndex(next);
        startStage(next);
        setPhase("intro");
        break;
      }
      case "over":
      case "win":
        beginRun();
        break;
      default:
        break;
    }
  }, [beginRun, stageIndex, startStage]);

  /* ---- input ----------------------------------------------------------- */

  useEffect(() => {
    function onDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onExit();
        return;
      }
      // The cabinet owns the keyboard while it is up: nothing here should also
      // scroll the page behind it or type into anything.
      const btn = BUTTONS[e.code];
      if (btn) {
        e.preventDefault();
        if (!e.repeat) tappedRef.current.add(btn);
        heldRef.current.add(btn);
        return;
      }
      if (e.code === "Enter" || e.code === "NumpadEnter") {
        e.preventDefault();
        if (phaseRef.current !== "play") advance();
        return;
      }
      if (e.code === "KeyP" && phaseRef.current === "play") {
        e.preventDefault();
        setPaused((v) => !v);
        return;
      }
      if (e.code === "KeyM") {
        e.preventDefault();
        setMuted((v) => {
          const next = !v;
          cabinetRef.current?.setMuted(next);
          setLegacySound(!next);
          return next;
        });
      }
    }

    function onUp(e: KeyboardEvent) {
      const btn = BUTTONS[e.code];
      if (btn) heldRef.current.delete(btn);
    }

    // Held buttons must not survive the window losing focus, or you come back
    // to a thread still walking left into a wall.
    function onBlur() {
      heldRef.current.clear();
      tappedRef.current.clear();
      if (phaseRef.current === "play") setPaused(true);
    }

    // Capture phase: the settings screen has its own document-level Escape
    // handler, and this one has to win.
    document.addEventListener("keydown", onDown, true);
    document.addEventListener("keyup", onUp, true);
    window.addEventListener("blur", onBlur);
    return () => {
      document.removeEventListener("keydown", onDown, true);
      document.removeEventListener("keyup", onUp, true);
      window.removeEventListener("blur", onBlur);
    };
  }, [advance, onExit]);

  /* ---- boot ------------------------------------------------------------ */

  useEffect(() => {
    if (phase !== "boot") return;
    cabinetRef.current?.power();
    const t = window.setTimeout(() => setPhase("title"), 1150);
    return () => window.clearTimeout(t);
  }, [phase]);

  /* ---- render ---------------------------------------------------------- */

  const showCanvas = phase === "play" || phase === "intro" || phase === "lost";
  const total = banked;

  const card = (() => {
    switch (phase) {
      case "title":
        return {
          kicker: "LEGACY CIRCUIT",
          heading: "A HIDDEN CABINET",
          body: CIRCUIT_TRIBUTE,
          foot: "PRESS ENTER TO BEGIN",
        };
      case "intro":
        return {
          kicker: `STAGE ${stageIndex + 1} OF ${STAGES.length}`,
          heading: stage.title,
          body: `${stage.brief}  ${stage.controls}.`,
          foot: "PRESS ENTER",
        };
      case "lost":
        return {
          kicker: "CIRCUIT BROKEN",
          heading: lives === 1 ? "LAST LIFE" : `${lives} LIVES LEFT`,
          body: "The stage restarts from the top. Your banked stages are safe.",
          foot: "PRESS ENTER",
        };
      case "stage-clear":
        return {
          kicker: `STAGE ${stageIndex + 1} CLEAR`,
          heading: stage.title,
          body: `${String(total).padStart(6, "0")} points banked. One more piece of the weave.`,
          foot: "PRESS ENTER",
        };
      case "over":
        return {
          kicker: "GAME OVER",
          heading: "THE THREAD SNAPS",
          body: `${String(peakRef.current).padStart(6, "0")} points. Three stages, three lives, one run: that is the deal.`,
          foot: "ENTER to try again · ESC to leave",
        };
      case "win":
        return {
          kicker: "ALL STAGES CLEAR",
          heading: CREST_NAME,
          body: `You found the circuit. You finished the run. Carry the thread forward. ${CIRCUIT_TRIBUTE}`,
          foot: "ENTER to run it again · ESC to leave",
        };
      default:
        return null;
    }
  })();

  return (
    <div
      className={`lc-frame${calm ? " calm" : ""}${phase === "boot" ? " booting" : ""}`}
      ref={frameRef}
      tabIndex={-1}
      role="dialog"
      aria-label="Legacy Circuit"
    >
      <div className="lc-glass" aria-hidden="true" />

      {/* One wrapper so the tube powers on as a single picture: animating the
          header, HUD and stage separately reads as four things appearing, not
          as a screen coming to life. */}
      <div className="lc-inner">
      <header className="lc-head">
        <span className="lc-marquee">LEGACY CIRCUIT</span>
        <span className="lc-head-stage">
          {phase === "boot" || phase === "title"
            ? "ATTRACT MODE"
            : `${stage.title} · ${stageIndex + 1}/${STAGES.length}`}
        </span>
        <button
          type="button"
          className="lc-chip"
          aria-pressed={!muted}
          onClick={() => {
            const next = !muted;
            setMuted(next);
            cabinetRef.current?.setMuted(next);
            setLegacySound(!next);
          }}
        >
          {muted ? "SOUND OFF" : "SOUND ON"}
        </button>
        <button type="button" className="lc-chip" onClick={onExit}>
          EXIT
        </button>
      </header>

      <div className="lc-hud">
        <span className="lc-hud-cell">
          <span className="lc-hud-key">SCORE</span>
          <span className="lc-hud-val" ref={scoreRef}>
            {String(total).padStart(6, "0")}
          </span>
        </span>
        <span className="lc-hud-cell">
          <span className="lc-hud-key">BEST</span>
          <span className="lc-hud-val">{String(best).padStart(6, "0")}</span>
        </span>
        <span className="lc-hud-cell">
          <span className="lc-hud-key">LIVES</span>
          <span className="lc-lives" aria-label={`${lives} lives left`}>
            {Array.from({ length: LIVES }, (_, i) => (
              <Crest key={i} size={11} className={i < lives ? "" : "spent"} />
            ))}
          </span>
        </span>
        <span className="lc-hud-cell wide">
          <span className="lc-hud-key">STATUS</span>
          <span className="lc-hud-val" ref={readoutRef}>
            {runRef.current?.readout() ?? "STANDBY"}
          </span>
        </span>
      </div>

      <div className="lc-stage" ref={stageWrapRef}>
        <canvas
          className={`lc-canvas${showCanvas ? "" : " hidden"}`}
          ref={canvasRef}
          width={VIEW_W}
          height={VIEW_H}
          style={{ width: VIEW_W * scale, height: VIEW_H * scale }}
          aria-hidden="true"
        />

        {card && (
          <div className={`lc-card lc-card-${phase}`} role="status" aria-live="polite">
            <div className="lc-card-kicker">{card.kicker}</div>
            {phase === "win" && <Crest size={54} className="lc-card-crest" title={CREST_NAME} />}
            <div className="lc-card-heading">{card.heading}</div>
            <p className="lc-card-body">{card.body}</p>
            <div className="lc-card-foot">{card.foot}</div>
          </div>
        )}

        {phase === "play" && paused && (
          <div className="lc-card lc-card-paused" role="status" aria-live="polite">
            <div className="lc-card-heading">PAUSED</div>
            <div className="lc-card-foot">P to resume</div>
          </div>
        )}
      </div>

      <footer className="lc-foot">
        <span>ESC leave · P pause · M sound</span>
        <span className="lc-foot-note">
          Original games. No part of this reproduces another title's art, audio or characters.
        </span>
      </footer>
      </div>
    </div>
  );
}
