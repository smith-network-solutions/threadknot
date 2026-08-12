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
import { createPortal } from "react-dom";
import {
  attemptCooldownRemaining,
  attemptsRemaining,
  awardLegacyCrest,
  awardPerfectClear,
  CIRCUIT_TRIBUTE,
  clampZoom,
  CREST_NAME,
  formatCooldown,
  getLegacyAward,
  markLegacyAttempt,
  MAX_ATTEMPTS,
  PERFECT_NAME,
  recordLegacyScore,
  setLegacyHandle,
  setLegacySound,
  setLegacyZoom,
  ZOOM_STEP,
} from "../../lib/legacyCircuit";
import { COOLDOWN, GAME_OVER, LIFE_LOST, pickTaunt } from "./taunts";
import { ensureFontLoaded } from "../../lib/fonts";
import { Cabinet } from "./sfx";
import { Crest } from "./Crest";
import { Witness, type WitnessReport } from "./attest";
import {
  LEVELS_PER_STAGE,
  STAGES,
  VIEW_H,
  VIEW_W,
  type Btn,
  type Input,
  type StageRun,
} from "./stages";
import { TRIBUTES } from "./tributes";
import "../../styles/legacy.css";

type Phase =
  /** The tube warming up. */
  | "boot"
  /** Marquee, dedication, insert coin. */
  | "title"
  /** Who is playing. Asked once per visit to the cabinet. */
  | "handle"
  /** One run an hour, and this one is inside the hour. */
  | "cooldown"
  /** Attract card for the stage about to start. */
  | "intro"
  | "play"
  /** A level dropped. There is no retry: you carry the loss to the next one. */
  | "lost"
  /** A level beaten, with more of this game still to come. */
  | "level-clear"
  /** A whole game beaten, and the page of names it buys you. */
  | "tribute"
  /** Out of lives. */
  | "over"
  /** All three cleared. */
  | "win";

const HANDLE_MAX = 14;

/** Keep a handle to something that will fit the marquee and cannot smuggle
 *  markup or control characters into a stored record. */
function cleanHandle(raw: string): string {
  return raw
    .replace(/[^A-Za-z0-9 _.\-]/g, "")
    .replace(/\s+/g, " ")
    .trimStart()
    .slice(0, HANDLE_MAX);
}

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
  KeyX: "bomb",
  KeyB: "bomb",
  KeyK: "bomb",
};

/** Levels you must win, out of the three in a game, to move on to the next.
 *  You may drop exactly one per game, and no level is ever replayed. */
const WINS_NEEDED = 2;

/** The result of each level in the game currently being played. */
type Card = ("win" | "loss" | null)[];
const freshCard = (): Card => [null, null, null];
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
  const [levelIndex, setLevelIndex] = useState(0);
  const [scoreCard, setScoreCard] = useState<Card>(freshCard);
  const [banked, setBanked] = useState(0);
  const [muted, setMuted] = useState(() => !getLegacyAward().sound);
  const [paused, setPaused] = useState(false);
  const [best, setBest] = useState(() => getLegacyAward().bestScore);
  const [scale, setScale] = useState(2);
  // Prefilled from the last visit: asking again is fine, retyping is not.
  const [handle, setHandle] = useState(() => getLegacyAward().handle);
  const [zoom, setZoom] = useState(() => getLegacyAward().zoom);

  const stage = STAGES[Math.min(stageIndex, STAGES.length - 1)];

  const frameRef = useRef<HTMLDivElement | null>(null);
  const stageWrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const readoutRef = useRef<HTMLSpanElement | null>(null);
  const scoreRef = useRef<HTMLSpanElement | null>(null);
  const handleInputRef = useRef<HTMLInputElement | null>(null);

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
  /** The stage measurer, so a zoom change can re-fit without the layout having
   *  moved (a ResizeObserver only ever fires on layout). */
  const measureRef = useRef<(() => void) | null>(null);
  const zoomRef = useRef(zoom);
  /** Set false the moment a life is lost. The Perfect Clear badge is the only
   *  thing that reads it, and nothing in the run can set it back to true. */
  const flawlessRef = useRef(true);
  /** The current game's card, mirrored for the loop: it has to read and write
   *  it inside a frame, before React has committed anything. */
  const cardRef = useRef<Card>(freshCard());
  /** Watches the shape of the input for this campaign; replaced at the start of
   *  every run so a perfect campaign is judged only on its own keystrokes. */
  const witnessRef = useRef<Witness>(new Witness());
  const [perfectReport, setPerfectReport] = useState<WitnessReport | null>(null);
  /** The line the cabinet is currently teasing you with, held in state so it
   *  does not reshuffle on every unrelated re-render mid-read. */
  const [taunt, setTaunt] = useState("");
  const [cooldownMs, setCooldownMs] = useState(0);
  const [credits, setCredits] = useState(() => attemptsRemaining());
  /** Drives the shake-and-tumble the moment a life goes. */
  const [dying, setDying] = useState(false);

  zoomRef.current = zoom;
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
      // clientWidth/Height, NOT getBoundingClientRect: the rect is the
      // TRANSFORMED box, and the tube power-on animates an ancestor down to
      // 0.6% height. Measuring during the boot therefore reported a stage six
      // pixels tall, pinned the scale at 1, and left it there forever, because
      // the layout size never changed and the observer never fired again.
      const w = wrap.clientWidth;
      const h = wrap.clientHeight;
      if (w < 4 || h < 4) return;
      // Fill the stage by default, then apply the player's zoom on top. Still
      // whole numbers only: a 2.5x canvas is a blurry canvas, and the whole
      // point of a 320x240 buffer is that a pixel stays a pixel.
      const fit = Math.min(w / VIEW_W, h / VIEW_H);
      setScale(Math.max(1, Math.min(8, Math.round(fit * zoomRef.current))));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(wrap);
    measureRef.current = measure;
    return () => {
      ro.disconnect();
      measureRef.current = null;
    };
  }, []);

  // Re-fit whenever the zoom changes; the observer only fires on layout.
  useEffect(() => {
    measureRef.current?.();
  }, [zoom]);

  /* ---- zoom ------------------------------------------------------------ */

  const nudgeZoom = useCallback((delta: number) => {
    setZoom((z) => {
      const next = clampZoom(z + delta);
      if (next !== z) setLegacyZoom(next);
      return next;
    });
  }, []);

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    function onWheel(e: WheelEvent) {
      // Ctrl (or Command) plus wheel is the zoom gesture everywhere else; take
      // it before the webview turns it into a page zoom.
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      nudgeZoom(e.deltaY < 0 ? ZOOM_STEP : -ZOOM_STEP);
    }
    // Not passive: the whole point is to preventDefault.
    frame.addEventListener("wheel", onWheel, { passive: false });
    return () => frame.removeEventListener("wheel", onWheel);
  }, [nudgeZoom]);

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
    (index: number, level: number) => {
      seedRef.current = (seedRef.current * 1664525 + 1013904223) >>> 0;
      runRef.current = STAGES[index].create(seedRef.current, level);
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

      if (settledRef.current) return;
      const won = run.status === "cleared";
      const lost = run.status === "failed";
      if (!won && !lost) return;

      settledRef.current = true;
      const mark: Card = [...cardRef.current];
      mark[levelIndex] = won ? "win" : "loss";
      cardRef.current = mark;
      setScoreCard(mark);

      if (won) {
        setBanked((s) => s + run.score);
      } else {
        // One dropped level is all it takes: Perfect Clear is meant to be rare.
        flawlessRef.current = false;
        setDying(true);
        cabinetRef.current?.death();
      }

      const isFinalLevel = levelIndex >= LEVELS_PER_STAGE - 1;
      if (!isFinalLevel) {
        setTaunt((prev) => (won ? prev : pickTaunt(LIFE_LOST, prev)));
        setPhase(won ? "level-clear" : "lost");
        return;
      }
      // Three played. Two wins carries the game; one does not.
      const wins = mark.filter((r) => r === "win").length;
      if (wins >= WINS_NEEDED) {
        setPhase("tribute");
      } else {
        setTaunt((prev) => pickTaunt(GAME_OVER, prev));
        setPhase("over");
      }
    };

    rafRef.current = requestAnimationFrame(tick);
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    };
    // `levelIndex` decides whether a result ends the game. It holds still for
    // the length of a level, so re-arming the loop when it changes is correct
    // and costs one frame.
  }, [phase, levelIndex]);

  /* ---- outcomes -------------------------------------------------------- */

  useEffect(() => {
    if (phase === "win") {
      const total = bankedRef.current;
      peakRef.current = Math.max(peakRef.current, total);
      const award = awardLegacyCrest(total);
      setBest(award.bestScore);
      cabinetRef.current?.fanfare();
      // Nine levels, no life lost. Judge the input that produced it and record
      // both facts together: the badge without the verdict would be a claim
      // nobody could check.
      if (flawlessRef.current) {
        const report = witnessRef.current.report();
        setPerfectReport(report);
        awardPerfectClear(report.verdict === "human", handle);
      } else {
        setPerfectReport(null);
      }
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

  // The death throes last about a second, then the card settles.
  useEffect(() => {
    if (!dying) return;
    const t = window.setTimeout(() => setDying(false), calm ? 1 : 1000);
    return () => window.clearTimeout(t);
  }, [dying, calm]);

  // Live countdown while the cabinet is cooling off, and an automatic release
  // back to the title the moment the hour is up.
  useEffect(() => {
    if (phase !== "cooldown") return;
    const update = () => {
      const left = attemptCooldownRemaining();
      setCooldownMs(left);
      if (left <= 0) {
        setCredits(attemptsRemaining());
        setPhase("title");
      }
    };
    update();
    const id = window.setInterval(update, 1000);
    return () => window.clearInterval(id);
  }, [phase]);

  const beginRun = useCallback(() => {
    // A credit is spent on commitment, not on curiosity. Charged here so
    // leaving mid-run cannot buy a free retry.
    markLegacyAttempt();
    setCredits(attemptsRemaining());
    setBanked(0);
    bankedRef.current = 0;
    cardRef.current = freshCard();
    setScoreCard(cardRef.current);
    setStageIndex(0);
    setLevelIndex(0);
    peakRef.current = 0;
    flawlessRef.current = true;
    witnessRef.current = new Witness();
    setPerfectReport(null);
    startStage(0, 0);
    setPhase("intro");
  }, [startStage]);

  /** Enter on any card: whatever the obvious next thing is. */
  const advance = useCallback(() => {
    cabinetRef.current?.arm();
    switch (phaseRef.current) {
      case "title":
        // Who is playing, before anything else happens. Once per visit: a
        // replay after a game over keeps the handle you already gave.
        if (attemptCooldownRemaining() > 0) {
          setTaunt((prev) => pickTaunt(COOLDOWN, prev));
          setPhase("cooldown");
        } else {
          setPhase("handle");
        }
        break;
      case "intro":
        setPaused(false);
        setPhase("play");
        break;
      // Won or dropped, the next level is the next level. Nothing is replayed:
      // the card is what decides whether the game carries.
      case "lost":
      case "level-clear": {
        const next = levelIndex + 1;
        setLevelIndex(next);
        startStage(stageIndex, next);
        setPhase("intro");
        break;
      }
      case "tribute": {
        const next = stageIndex + 1;
        if (next >= STAGES.length) {
          setPhase("win");
          break;
        }
        setStageIndex(next);
        setLevelIndex(0);
        // A fresh game, a fresh card. Carrying a dropped level into a game you
        // have not seen yet is a punishment for progress.
        cardRef.current = freshCard();
        setScoreCard(cardRef.current);
        startStage(next, 0);
        setPhase("intro");
        break;
      }
      case "over":
      case "win":
        // The run is spent. Another one has to wait for the hour, which is the
        // whole point of a machine that only takes one coin at a time.
        if (attemptCooldownRemaining() > 0) {
          setTaunt((prev) => pickTaunt(COOLDOWN, prev));
          setPhase("cooldown");
        } else {
          beginRun();
        }
        break;
      default:
        break;
    }
  }, [beginRun, stageIndex, levelIndex, startStage]);

  /** Leaving the handle screen: store it and start stage one. */
  const submitHandle = useCallback(() => {
    const clean = cleanHandle(handle).trim();
    const final = clean || "ANON";
    setHandle(final);
    setLegacyHandle(final);
    cabinetRef.current?.arm();
    beginRun();
  }, [handle, beginRun]);

  /* ---- input ----------------------------------------------------------- */

  useEffect(() => {
    function onDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onExit();
        return;
      }
      // Zoom by keyboard as well as by wheel, using the shortcuts every other
      // program uses. Checked before the handle guard so it works there too.
      if (e.ctrlKey || e.metaKey) {
        if (e.code === "Equal" || e.code === "NumpadAdd") {
          e.preventDefault();
          nudgeZoom(ZOOM_STEP);
          return;
        }
        if (e.code === "Minus" || e.code === "NumpadSubtract") {
          e.preventDefault();
          nudgeZoom(-ZOOM_STEP);
          return;
        }
        if (e.code === "Digit0" || e.code === "Numpad0") {
          e.preventDefault();
          setZoom(1);
          setLegacyZoom(1);
          return;
        }
      }
      // While the handle is being typed the keyboard belongs to the field, not
      // to the cabinet. Without this, every letter that happens to be a game
      // button would be swallowed before it could be typed.
      if (phaseRef.current === "handle") return;
      // The cabinet owns the keyboard while it is up: nothing here should also
      // scroll the page behind it or type into anything.
      const btn = BUTTONS[e.code];
      if (btn) {
        e.preventDefault();
        // Auto-repeat is the keyboard talking, not the player, so it is not
        // evidence either way and must not be counted as rhythm.
        if (!e.repeat) {
          witnessRef.current.keyDown(e.code, e.isTrusted, e.timeStamp);
          tappedRef.current.add(btn);
        }
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
      if (btn) {
        witnessRef.current.keyUp(e.code, e.isTrusted, e.timeStamp);
        heldRef.current.delete(btn);
      }
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
  }, [advance, onExit, nudgeZoom]);

  /* ---- boot ------------------------------------------------------------ */

  useEffect(() => {
    if (phase !== "boot") return;
    cabinetRef.current?.power();
    const t = window.setTimeout(() => setPhase("title"), 1150);
    return () => window.clearTimeout(t);
  }, [phase]);

  // Walking in during the cooling hour: say so at the door rather than letting
  // someone type a handle and only then find out.
  useEffect(() => {
    if (phase !== "title") return;
    if (attemptCooldownRemaining() <= 0) return;
    setTaunt((prev) => pickTaunt(COOLDOWN, prev));
    setPhase("cooldown");
  }, [phase]);

  // The handle field wants the caret the moment it appears, and the boot
  // animation's transform is over by any phase that shows it.
  useEffect(() => {
    if (phase !== "handle") return;
    const input = handleInputRef.current;
    input?.focus();
    input?.select();
  }, [phase]);

  /* ---- render ---------------------------------------------------------- */

  const showCanvas =
    phase === "play" || phase === "intro" || phase === "lost" || phase === "over";
  const total = banked;

  const card = (() => {
    switch (phase) {
      case "title":
        return {
          kicker: "LEGACY CIRCUIT",
          heading: "A HIDDEN CABINET",
          body: `Three games, three levels each. Take ${WINS_NEEDED} of the 3 to carry a game. No retries. ${credits} of ${MAX_ATTEMPTS} credits left.`,
          // The reason the whole thing exists, on the first screen anyone who
          // finds their way in will see.
          dedication: CIRCUIT_TRIBUTE,
          foot: "PRESS ENTER TO BEGIN",
        };
      case "intro":
        return {
          kicker: `GAME ${stageIndex + 1} OF ${STAGES.length} · LEVEL ${levelIndex + 1} OF ${LEVELS_PER_STAGE}`,
          heading: stage.title,
          body: `${stage.brief}  ${stage.levelNote(levelIndex)}  ${stage.controls}.`,
          foot: "PRESS ENTER",
        };
      case "level-clear":
        return {
          kicker: `LEVEL ${levelIndex + 1} CLEAR · ${String(total).padStart(6, "0")} BANKED`,
          heading: `LEVEL ${levelIndex + 2}`,
          body: stage.levelNote(levelIndex + 1),
          foot: "PRESS ENTER",
        };
      case "lost": {
        const dropped = scoreCard.filter((r) => r === "loss").length;
        const left = LEVELS_PER_STAGE - scoreCard.filter(Boolean).length;
        return {
          kicker: `LEVEL ${levelIndex + 1} DROPPED`,
          heading: dropped >= 2 ? "THAT IS THE GAME" : "ONE DROPPED",
          taunt,
          body:
            dropped >= 2
              ? `Two dropped out of three. You need ${WINS_NEEDED} to carry a game, so the rest of this one is a formality.`
              : `No retries here: the next level is the next level. You need ${WINS_NEEDED} wins from three to carry this game, and you have ${left} left to take.`,
          foot: "PRESS ENTER",
        };
      }
      case "cooldown":
        return {
          kicker: `CABINET COOLING · ${MAX_ATTEMPTS} CREDITS SPENT`,
          heading: formatCooldown(cooldownMs),
          taunt,
          body:
            `${MAX_ATTEMPTS} runs, then the machine wants an hour to itself. That is not a bug, it is ` +
            "the era: you spent your change, and then you stood and watched somebody better than you " +
            `take a turn. All ${MAX_ATTEMPTS} credits come back when the clock runs out.`,
          foot: "ESC to leave",
        };
      case "tribute": {
        const t = TRIBUTES[Math.min(stageIndex, TRIBUTES.length - 1)];
        const wins = scoreCard.filter((r) => r === "win").length;
        return {
          kicker: `${t.kicker} · ${wins}/${LEVELS_PER_STAGE} TAKEN · ${String(total).padStart(6, "0")} BANKED`,
          heading: t.heading,
          body: t.lede,
          roll: t.roll,
          close: t.close,
          foot: "PRESS ENTER",
        };
      }
      case "over":
        return {
          kicker: `GAME OVER · ${scoreCard.filter((r) => r === "win").length}/${LEVELS_PER_STAGE} ON ${stage.title}`,
          heading: "THE THREAD SNAPS",
          taunt,
          body:
            `${handle || "PLAYER"}, ${String(peakRef.current).padStart(6, "0")} points. ` +
            (credits > 0
              ? `${credits} credit${credits === 1 ? "" : "s"} left. Go again.`
              : `That was the last of ${MAX_ATTEMPTS}. The machine wants an hour now.`),
          foot: credits > 0 ? "ENTER to spend another credit · ESC to leave" : "ENTER for the clock · ESC to leave",
        };
      case "win":
        return {
          kicker: `ALL NINE LEVELS CLEAR · ${String(total).padStart(6, "0")}`,
          heading: CREST_NAME,
          body: `${handle || "PLAYER"}, you found the circuit. You finished the run. Carry the thread forward.`,
          dedication: CIRCUIT_TRIBUTE,
          foot: "ENTER to run it again · ESC to leave",
        };
      default:
        return null;
    }
  })();

  // Portaled to <body> and sized against the viewport, not against the settings
  // dialog it was opened from. A cabinet is the only thing on the screen while
  // you are at it, and 880 by 660 is a postcard to play a game through.
  return createPortal(
    <div className="lc-backdrop">
    <div
      className={`lc-frame${calm ? " calm" : ""}${phase === "boot" ? " booting" : ""}${
        dying ? " dying" : ""
      }`}
      ref={frameRef}
      tabIndex={-1}
      role="dialog"
      aria-label="Legacy Circuit"
      // Every type size in the cabinet is multiplied by this, so one number
      // scales the whole machine rather than each rule needing its own knob.
      style={{ "--lc-zoom": String(zoom) } as React.CSSProperties}
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
        <span className="lc-zoom">
          <button
            type="button"
            className="lc-chip lc-zoom-btn"
            aria-label="Zoom out"
            onClick={() => nudgeZoom(-ZOOM_STEP)}
          >
            −
          </button>
          <button
            type="button"
            className="lc-chip lc-zoom-level"
            title="Ctrl + scroll, or Ctrl + 0 to reset"
            aria-label={`Zoom ${Math.round(zoom * 100)} percent, click to reset`}
            onClick={() => {
              setZoom(1);
              setLegacyZoom(1);
            }}
          >
            {Math.round(zoom * 100)}%
          </button>
          <button
            type="button"
            className="lc-chip lc-zoom-btn"
            aria-label="Zoom in"
            onClick={() => nudgeZoom(ZOOM_STEP)}
          >
            +
          </button>
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
        {handle && (
          <span className="lc-hud-cell">
            <span className="lc-hud-key">PLAYER</span>
            <span className="lc-hud-val lc-hud-handle">{handle.toUpperCase()}</span>
          </span>
        )}
        <span className="lc-hud-cell">
          <span className="lc-hud-key">CREDITS</span>
          <span className={`lc-hud-val lc-hud-credits${credits === 0 ? " spent" : ""}`}>
            {credits}/{MAX_ATTEMPTS}
          </span>
        </span>
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
          <span className="lc-hud-key">CARD</span>
          <span
            className="lc-card-marks"
            aria-label={`This game: ${scoreCard.filter((r) => r === "win").length} won, ${scoreCard.filter((r) => r === "loss").length} dropped, ${WINS_NEEDED} needed`}
          >
            {scoreCard.map((r, i) => (
              <span
                key={i}
                className={`lc-mark${r ? ` ${r}` : ""}${i === levelIndex && !r ? " now" : ""}`}
              >
                {r === "win" ? "W" : r === "loss" ? "X" : "·"}
              </span>
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
        <div
          className={`lc-screen${showCanvas ? "" : " hidden"}${dying ? " dying" : ""}`}
          style={{ width: VIEW_W * scale, height: VIEW_H * scale }}
        >
          <canvas
            className="lc-canvas"
            ref={canvasRef}
            width={VIEW_W}
            height={VIEW_H}
            style={{ width: VIEW_W * scale, height: VIEW_H * scale }}
            aria-hidden="true"
          />
        </div>

        {/* Every card centres its content when it fits and scrolls when it does
            not. The inner wrapper carries `margin: auto` rather than the card
            using justify-content:center, because a centred flex child that
            overflows puts its own top out of reach of the scrollbar. */}
        {card && (
          <div className={`lc-card lc-card-${phase}`} role="status" aria-live="polite">
            <div className="lc-card-inner">
              <div className="lc-card-kicker">{card.kicker}</div>
              {phase === "win" && <Crest size={72} className="lc-card-crest" title={CREST_NAME} />}
              {(phase === "lost" || phase === "over") && (
                <Crest size={64} variant="dead" className="lc-card-dead" />
              )}
              <div className="lc-card-heading">{card.heading}</div>
              {/* The tease sits above the explanation: it is the first thing
                  you want to read and the last thing you want to hunt for. */}
              {"taunt" in card && card.taunt && (
                <p className="lc-card-taunt">{card.taunt}</p>
              )}
              <p className="lc-card-body">{card.body}</p>
              {"dedication" in card && card.dedication && (
                <p className="lc-card-dedication">{card.dedication}</p>
              )}
              {"roll" in card && card.roll && (
                <ul className="lc-roll">
                  {card.roll.map((entry) => (
                    <li className="lc-roll-item" key={entry.name}>
                      <span className="lc-roll-name">{entry.name}</span>
                      <span className="lc-roll-note">{entry.note}</span>
                    </li>
                  ))}
                </ul>
              )}
              {"close" in card && card.close && (
                <p className="lc-card-close">{card.close}</p>
              )}

              {/* The rare one. Only on the winning screen, only when nothing
                  was lost, and always shown with what the verification does
                  and does not establish. */}
              {phase === "win" && perfectReport && (
                <div
                  className={`lc-perfect${perfectReport.verdict === "human" ? " verified" : ""}`}
                >
                  <Crest size={40} variant="perfect" title={PERFECT_NAME} />
                  <div className="lc-perfect-text">
                    <span className="lc-perfect-name">{PERFECT_NAME}</span>
                    <span className="lc-perfect-sub">
                      Nine levels. Not one life lost.
                    </span>
                    <span className="lc-perfect-verdict">
                      {perfectReport.verdict === "human"
                        ? `Input looked human: ${perfectReport.events} keystrokes, timing spread ${perfectReport.intervalCv}. That is a heuristic, not proof.`
                        : `Not verified as human input: ${perfectReport.reasons.join("; ")}.`}
                    </span>
                  </div>
                </div>
              )}

              <div className="lc-card-foot">{card.foot}</div>
            </div>
          </div>
        )}

        {phase === "handle" && (
          <div className="lc-card lc-card-handle" role="dialog" aria-label="Enter your handle">
            <div className="lc-card-inner">
              <div className="lc-card-kicker">IDENTIFY YOURSELF</div>
              <div className="lc-card-heading">ENTER YOUR HANDLE</div>
              <p className="lc-card-body">
                It goes on the cabinet, and nowhere else. This machine remembers
                it for next time and never sends it anywhere.
              </p>
              <form
                className="lc-handle-form"
                onSubmit={(e) => {
                  e.preventDefault();
                  submitHandle();
                }}
              >
                <input
                  ref={handleInputRef}
                  className="lc-handle-input"
                  type="text"
                  inputMode="text"
                  autoComplete="off"
                  spellCheck={false}
                  maxLength={HANDLE_MAX}
                  aria-label="Your handle"
                  placeholder="WHO GOES THERE"
                  value={handle}
                  onChange={(e) => setHandle(cleanHandle(e.target.value))}
                />
                <button type="submit" className="lc-handle-go">
                  START
                </button>
              </form>
              <div className="lc-card-foot">ENTER to begin · ESC to leave</div>
            </div>
          </div>
        )}

        {phase === "play" && paused && (
          <div className="lc-card lc-card-paused" role="status" aria-live="polite">
            <div className="lc-card-inner">
              <div className="lc-card-heading">PAUSED</div>
              <div className="lc-card-foot">P to resume</div>
            </div>
          </div>
        )}
      </div>

      <footer className="lc-foot">
        <span>ESC leave · P pause · M sound · X bomb</span>
        <span className="lc-foot-note">
          Original games. No part of this reproduces another title's art, audio or characters.
        </span>
      </footer>
      </div>
    </div>
    </div>,
    document.body,
  );
}
