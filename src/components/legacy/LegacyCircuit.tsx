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
  awardLegacyCrest,
  CIRCUIT_TRIBUTE,
  CREST_NAME,
  getLegacyAward,
  recordLegacyScore,
  setLegacyHandle,
  setLegacySound,
} from "../../lib/legacyCircuit";
import { ensureFontLoaded } from "../../lib/fonts";
import { Cabinet } from "./sfx";
import { Crest } from "./Crest";
import { STAGES, VIEW_H, VIEW_W, type Btn, type Input, type StageRun } from "./stages";
import { TRIBUTES } from "./tributes";
import "../../styles/legacy.css";

type Phase =
  /** The tube warming up. */
  | "boot"
  /** Marquee, dedication, insert coin. */
  | "title"
  /** Who is playing. Asked once per visit to the cabinet. */
  | "handle"
  /** Attract card for the stage about to start. */
  | "intro"
  | "play"
  /** A life gone; the stage restarts from the top. */
  | "lost"
  /** A stage beaten, and the page of names it buys you. */
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
  // Prefilled from the last visit: asking again is fine, retyping is not.
  const [handle, setHandle] = useState(() => getLegacyAward().handle);

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
      // Up to 6x now that the cabinet owns the window, with every pixel still
      // landing on a whole number of real ones.
      setScale(Math.max(1, Math.min(6, Math.floor(Math.min(w / VIEW_W, h / VIEW_H)))));
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
        // Every cleared stage goes to its tribute page, including the last.
        // The crest screen comes after the third one.
        setPhase("tribute");
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
        // Who is playing, before anything else happens. Once per visit: a
        // replay after a game over keeps the handle you already gave.
        setPhase("handle");
        break;
      case "intro":
        setPaused(false);
        setPhase("play");
        break;
      case "lost":
        startStage(stageIndex);
        setPhase("intro");
        break;
      case "tribute": {
        const next = stageIndex + 1;
        if (next >= STAGES.length) {
          setPhase("win");
          break;
        }
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
      // While the handle is being typed the keyboard belongs to the field, not
      // to the cabinet. Without this, every letter that happens to be a game
      // button would be swallowed before it could be typed.
      if (phaseRef.current === "handle") return;
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

  // The handle field wants the caret the moment it appears, and the boot
  // animation's transform is over by any phase that shows it.
  useEffect(() => {
    if (phase !== "handle") return;
    const input = handleInputRef.current;
    input?.focus();
    input?.select();
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
          body: "Three stages. Three lives. One run.",
          // The reason the whole thing exists, on the first screen anyone who
          // finds their way in will see.
          dedication: CIRCUIT_TRIBUTE,
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
      case "tribute": {
        const t = TRIBUTES[Math.min(stageIndex, TRIBUTES.length - 1)];
        return {
          kicker: `${t.kicker} · ${String(total).padStart(6, "0")} BANKED`,
          heading: t.heading,
          body: t.lede,
          roll: t.roll,
          close: t.close,
          foot: "PRESS ENTER",
        };
      }
      case "over":
        return {
          kicker: "GAME OVER",
          heading: "THE THREAD SNAPS",
          body: `${handle || "PLAYER"}, ${String(peakRef.current).padStart(6, "0")} points. Three stages, three lives, one run: that is the deal.`,
          foot: "ENTER to try again · ESC to leave",
        };
      case "win":
        return {
          kicker: `ALL STAGES CLEAR · ${String(total).padStart(6, "0")}`,
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
        {handle && (
          <span className="lc-hud-cell">
            <span className="lc-hud-key">PLAYER</span>
            <span className="lc-hud-val lc-hud-handle">{handle.toUpperCase()}</span>
          </span>
        )}
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
            <div className="lc-card-foot">{card.foot}</div>
          </div>
        )}

        {phase === "handle" && (
          <div className="lc-card lc-card-handle" role="dialog" aria-label="Enter your handle">
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
        )}

        {phase === "play" && paused && (
          <div className="lc-card lc-card-paused" role="status" aria-live="polite">
            <div className="lc-card-heading">PAUSED</div>
            <div className="lc-card-foot">P to resume</div>
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
